//! Clip dictionaries (`.ycd`): the animations a ped, vehicle or prop plays, and the clips
//! (named time ranges, tags and properties) that point into them.
//!
//! A dictionary is read into plain structs ([`parse_ycd`]), written back to the binary
//! resource ([`serialize_ycd`]), and converted to and from the `.ycd.xml` text form
//! ([`to_xml`], [`from_xml`]) that other tools (Sollumz, CodeWalker) use.
//!
//! An animation is a list of *sequences*, each covering up to the animation's
//! `sequence_frame_limit` frames. A sequence holds one *sequence data* entry per bone-track in
//! the animation's `bone_ids`, and each entry is a few *channels* (the x, y and z of a
//! position, say) packed in one of nine encodings. The binary form groups the channels by
//! encoding rather than by bone, so reading and writing both walk four regions of one data
//! block: the per-channel header data, the per-frame bit stream, the channel counts and the
//! channel-to-sequence table.

use std::collections::HashMap;

use anyhow::{anyhow, bail, ensure, Context, Result};

use crate::resource::{f32_le, prepare_rsc7, u16_le, u32_le, u64_le, ResReader, SYSTEM_BASE};

/// The resource version a `.ycd` carries in its header.
pub const YCD_VERSION: u32 = 46;

// ─── model ───────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClipDictionary {
    pub clips: Vec<Clip>,
    pub animations: Vec<Animation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub hash: u32,
    pub name: String,
    pub unknown30: u32,
    pub tags: Vec<Tag>,
    pub properties: Vec<Property>,
    pub kind: ClipKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClipKind {
    /// Plays one animation between two times.
    Animation(ClipAnimation),
    /// Plays several animations at once for `duration` seconds.
    List { duration: f32, animations: Vec<ClipAnimation> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipAnimation {
    pub animation: u32,
    pub start: f32,
    pub end: f32,
    pub rate: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: u32,
    pub unk: u32,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub property: Property,
    pub start_phase: f32,
    pub end_phase: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: u32,
    pub value: AttrValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Float(f32),
    Int(i32),
    Bool(u32),
    String(String),
    Vector3 { value: [f32; 3], unknown2c: f32 },
    Vector4([f32; 4]),
    HashString(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub hash: u32,
    pub unknown10: u8,
    pub frames: u16,
    pub sequence_frame_limit: u16,
    pub duration: f32,
    pub unknown1c: u32,
    pub bone_ids: Vec<BoneId>,
    pub sequences: Vec<Sequence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoneId {
    pub id: u16,
    pub track: u8,
    pub unk0: u8,
}

/// The frames `[n * sequence_frame_limit, ...)` of an animation.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    pub hash: u32,
    pub frame_count: u16,
    /// One entry per bone-track of the animation, in `bone_ids` order.
    pub data: Vec<Vec<Channel>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Channel {
    StaticQuaternion([f32; 4]),
    StaticVector3([f32; 3]),
    StaticFloat(f32),
    RawFloat(Vec<f32>),
    /// One value per frame, stored as `round((value - offset) / quantum)` in as few bits as fit.
    QuantizeFloat { quantum: f32, offset: f32, values: Vec<f32> },
    /// A table of distinct `values`, and per frame the index of the one to use.
    IndirectQuantizeFloat { quantum: f32, offset: f32, values: Vec<f32>, frames: Vec<u32> },
    /// The fourth component of a quaternion, worked out from the other three. `kind` is 1 (three
    /// other channels) or 2 (four); `quat_index` says which component it replaces.
    CachedQuaternion { kind: u8, quat_index: u32 },
}

impl Channel {
    const TYPE_NAMES: [&'static str; 9] = [
        "StaticQuaternion", "StaticVector3", "StaticFloat", "RawFloat", "QuantizeFloat", "IndirectQuantizeFloat",
        "LinearFloat", "CachedQuaternion1", "CachedQuaternion2",
    ];

    /// The binary type id (0 to 8). LinearFloat (6) is read but never written back as such.
    pub(crate) fn type_id(&self) -> usize {
        match self {
            Channel::StaticQuaternion(_) => 0,
            Channel::StaticVector3(_) => 1,
            Channel::StaticFloat(_) => 2,
            Channel::RawFloat(_) => 3,
            Channel::QuantizeFloat { .. } => 4,
            Channel::IndirectQuantizeFloat { .. } => 5,
            Channel::CachedQuaternion { kind, .. } => 6 + *kind as usize,
        }
    }

    pub fn type_name(&self) -> &'static str {
        Self::TYPE_NAMES[self.type_id()]
    }
}

// ─── bit reading and writing ───────────────────────────────────────────────────────────────

/// Reads `len` bits (at most 32) starting at bit `start`, least significant bit first. Bytes past
/// the end read as zero.
fn get_bits(data: &[u8], start: i64, len: usize) -> u32 {
    if start < 0 || len == 0 {
        return 0;
    }
    let (byte, shift) = ((start / 8) as usize, (start % 8) as u32);
    let mut word = 0u64;
    for k in 0..8 {
        word |= (*data.get(byte + k).unwrap_or(&0) as u64) << (8 * k);
    }
    ((word >> shift) & ((1u64 << len) - 1)) as u32
}

/// A little-endian bit stream built from `u32` words.
#[derive(Default)]
pub(crate) struct BitWriter {
    pub(crate) words: Vec<u32>,
    pos: usize,
}

impl BitWriter {
    pub(crate) fn write(&mut self, bits: u32, n: usize) {
        if n == 0 {
            return;
        }
        let masked = (bits as u64) & ((1u64 << n) - 1);
        let (index, offset) = (self.pos / 32, self.pos % 32);
        while self.words.len() < (self.pos + n).div_ceil(32) {
            self.words.push(0);
        }
        let shifted = masked << offset;
        self.words[index] |= shifted as u32;
        if offset + n > 32 {
            self.words[index + 1] |= (shifted >> 32) as u32;
        }
        self.pos += n;
    }
}

pub(crate) fn bit_count(max: u32) -> usize {
    32 - max.leading_zeros() as usize
}

// ─── reading ───────────────────────────────────────────────────────────────────────────────

/// Reads a binary `.ycd`.
pub fn parse_ycd(data: &[u8]) -> Result<ClipDictionary> {
    let (system, graphics) = prepare_rsc7(data)?;
    let rd = ResReader { system: &system, graphics: &graphics };
    let root = rd.resolve(SYSTEM_BASE, 0x40).ok_or_else(|| anyhow!("clip dictionary: no root block"))?;

    // animations: AnimationMap -> bucket array -> chains of entries -> Animation
    let mut animations = Vec::new();
    let mut hash_of: HashMap<u64, u32> = HashMap::new(); // animation block address -> its hash
    let map = rd.resolve(u64_le(root, 0x18), 0x30).ok_or_else(|| anyhow!("clip dictionary: no animation map"))?;
    for entry_va in chain_entries(&rd, u64_le(map, 0x18), u16_le(map, 0x20) as usize)? {
        let entry = rd.resolve(entry_va, 0x20).context("animation map entry out of bounds")?;
        let hash = u32_le(entry, 0);
        let anim_va = u64_le(entry, 8);
        if anim_va == 0 {
            continue;
        }
        let a = parse_animation(&rd, anim_va, hash).with_context(|| format!("animation hash_{hash:08X}"))?;
        hash_of.insert(anim_va, hash);
        animations.push(a);
    }

    // clips: bucket array -> chains of entries -> ClipBase
    let mut clips = Vec::new();
    for entry_va in chain_entries(&rd, u64_le(root, 0x28), u16_le(root, 0x30) as usize)? {
        let entry = rd.resolve(entry_va, 0x20).context("clip map entry out of bounds")?;
        let hash = u32_le(entry, 0);
        let clip_va = u64_le(entry, 8);
        if clip_va == 0 {
            continue;
        }
        clips.push(parse_clip(&rd, clip_va, hash, &hash_of).with_context(|| format!("clip hash_{hash:08X}"))?);
    }
    Ok(ClipDictionary { clips, animations })
}

/// The entries of a hash table in the order the reference tool lists them: bucket by bucket,
/// each bucket's chain front to back. Entries are 32 bytes with the next pointer at 0x10.
fn chain_entries(rd: &ResReader, array_va: u64, capacity: usize) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    let Some(buckets) = rd.read_u64_list(array_va, capacity) else { bail!("hash table out of bounds") };
    for head in buckets {
        let mut va = head;
        let mut guard = 0;
        while va != 0 {
            out.push(va);
            let entry = rd.resolve(va, 0x20).context("hash table entry out of bounds")?;
            va = u64_le(entry, 0x10);
            guard += 1;
            ensure!(guard < 100_000, "hash table chain does not end");
        }
    }
    Ok(out)
}

fn parse_animation(rd: &ResReader, va: u64, hash: u32) -> Result<Animation> {
    let b = rd.resolve(va, 0x60).context("animation block out of bounds")?;
    let seq_list = rd.read_pointer_list_header(va + 0x40).context("sequence list")?;
    let bone_list = rd.read_pointer_list_header(va + 0x50).context("bone id list")?;
    let mut bone_ids = Vec::new();
    if bone_list.count > 0 {
        let raw = rd.resolve(bone_list.pointer, bone_list.count as usize * 4).context("bone ids out of bounds")?;
        for c in raw.chunks_exact(4) {
            bone_ids.push(BoneId { id: u16_le(c, 0), unk0: c[2], track: c[3] });
        }
    }
    let mut sequences = Vec::new();
    for ptr in rd.read_u64_list(seq_list.pointer, seq_list.count as usize).context("sequence pointers")? {
        sequences.push(parse_sequence(rd, ptr)?);
    }
    Ok(Animation {
        hash,
        unknown10: b[0x10],
        frames: u16_le(b, 0x14),
        sequence_frame_limit: u16_le(b, 0x16),
        duration: f32_le(b, 0x18),
        unknown1c: u32_le(b, 0x1C),
        bone_ids,
        sequences,
    })
}

/// A channel while its sequence's data block is being read.
struct Read {
    sequence: usize,
    index: usize,
    channel: Channel,
    /// Bits this channel takes in every frame.
    frame_bits: usize,
    /// Whether its values are in the frame bits. A linear channel is decoded whole, from its own data.
    in_frames: bool,
}

fn parse_sequence(rd: &ResReader, va: u64) -> Result<Sequence> {
    let head = rd.resolve(va, 0x20).context("sequence out of bounds")?;
    let data_len = u32_le(head, 4) as usize;
    let frame_offset = u32_le(head, 0x0C) as usize;
    let num_frames = u16_le(head, 0x16) as usize;
    let frame_length = u16_le(head, 0x18) as usize;
    let chunk_size = head[0x1E] as usize;
    let data = rd.resolve(va + 0x20, data_len).context("sequence data out of bounds")?;

    let mut list_at = frame_offset + frame_length * num_frames; // nine channel counts
    let mut item_at = list_at + 18; // then the channel-to-sequence words, four to a group
    ensure!(item_at <= data.len(), "sequence data too short for its channel table");
    let mut pos = 0usize; // into the per-channel data at the start of the block
    let mut chans: Vec<Read> = Vec::new();

    for kind in 0..9 {
        let count = u16_le(data, list_at) as usize;
        list_at += 2;
        for _ in 0..count {
            ensure!(item_at + 2 <= data.len(), "channel table runs past the data");
            let word = u16_le(data, item_at) as usize;
            item_at += 2;
            let (channel, frame_bits) = read_channel(kind, data, &mut pos, num_frames, chunk_size)?;
            let in_frames = kind != 6;
            let (sequence, mut index) = (word >> 2, word & 3);
            let channel = match channel {
                Channel::CachedQuaternion { kind: k, .. } => {
                    let quat_index = index as u32;
                    index = if k == 1 { 3 } else { 4 };
                    Channel::CachedQuaternion { kind: k, quat_index }
                }
                other => other,
            };
            chans.push(Read { sequence, index, channel, frame_bits, in_frames });
        }
        if count % 4 != 0 {
            item_at += (4 - count % 4) * 2;
        }
    }

    // the per-frame bits of every channel, frame by frame, channels in the order read
    for f in 0..num_frames {
        let mut bit = ((frame_offset + frame_length * f) * 8) as i64;
        for c in chans.iter_mut().filter(|c| c.in_frames) {
            match &mut c.channel {
                Channel::RawFloat(v) => v[f] = f32::from_bits(get_bits(data, bit, 32)),
                Channel::QuantizeFloat { quantum, offset, values } => {
                    values[f] = get_bits(data, bit, c.frame_bits) as f32 * *quantum + *offset
                }
                Channel::IndirectQuantizeFloat { frames, .. } => frames[f] = get_bits(data, bit, c.frame_bits),
                _ => {}
            }
            bit += c.frame_bits as i64;
        }
    }

    let n_seq = chans.iter().map(|c| c.sequence).max().map_or(0, |m| m + 1);
    let mut grouped: Vec<Vec<(usize, Channel)>> = vec![Vec::new(); n_seq];
    for c in chans {
        grouped[c.sequence].push((c.index, c.channel));
    }
    let mut seq_data = Vec::new();
    for mut g in grouped {
        g.sort_by_key(|(i, _)| *i);
        for (want, (have, _)) in g.iter().enumerate() {
            ensure!(want == *have, "a sequence is missing channel {want}");
        }
        seq_data.push(g.into_iter().map(|(_, c)| c).collect());
    }
    Ok(Sequence { hash: u32_le(head, 0), frame_count: num_frames as u16, data: seq_data })
}

/// Reads one channel's header from the per-channel data at `*pos` and returns it along with the
/// bits it takes in every frame. The per-frame values are filled in afterwards.
fn read_channel(kind: usize, data: &[u8], pos: &mut usize, frames: usize, chunk_size: usize) -> Result<(Channel, usize)> {
    let need = |pos: usize, n: usize| -> Result<()> {
        ensure!(pos + n <= data.len(), "channel data runs past the end of the block");
        Ok(())
    };
    let f32_at = |p: usize| f32_le(data, p);
    let i32_at = |p: usize| u32_le(data, p) as i32;
    Ok(match kind {
        0 => {
            need(*pos, 12)?;
            let (x, y, z) = (f32_at(*pos), f32_at(*pos + 4), f32_at(*pos + 8));
            *pos += 12;
            (Channel::StaticQuaternion([x, y, z, (1.0 - (x * x + y * y + z * z)).max(0.0).sqrt()]), 0)
        }
        1 => {
            need(*pos, 12)?;
            let v = [f32_at(*pos), f32_at(*pos + 4), f32_at(*pos + 8)];
            *pos += 12;
            (Channel::StaticVector3(v), 0)
        }
        2 => {
            need(*pos, 4)?;
            let v = f32_at(*pos);
            *pos += 4;
            (Channel::StaticFloat(v), 0)
        }
        3 => (Channel::RawFloat(vec![0.0; frames]), 32),
        4 => {
            need(*pos, 12)?;
            let (bits, quantum, offset) = (i32_at(*pos).clamp(0, 32) as usize, f32_at(*pos + 4), f32_at(*pos + 8));
            *pos += 12;
            (Channel::QuantizeFloat { quantum, offset, values: vec![0.0; frames] }, bits)
        }
        5 => {
            need(*pos, 20)?;
            let frame_bits = i32_at(*pos).clamp(0, 32) as usize;
            let value_bits = i32_at(*pos + 4).clamp(0, 32) as usize;
            let num_ints = i32_at(*pos + 8).max(0) as usize;
            let (quantum, offset) = (f32_at(*pos + 12), f32_at(*pos + 16));
            *pos += 20;
            let by_size = if value_bits > 0 { num_ints * 32 / value_bits } else { 0 };
            let by_frame_bits = ((1u64 << frame_bits) - 1) as usize;
            let count = by_size.min(by_frame_bits);
            let mut values = Vec::with_capacity(count);
            for i in 0..count {
                values.push(get_bits(data, (*pos * 8 + i * value_bits) as i64, value_bits) as f32 * quantum + offset);
            }
            *pos += num_ints * 4;
            (Channel::IndirectQuantizeFloat { quantum, offset, values, frames: vec![0; frames] }, frame_bits)
        }
        6 => {
            // read, then reported as a QuantizeFloat: this tool never writes one back
            need(*pos, 16)?;
            let (num_ints, counts) = (i32_at(*pos) as usize, i32_at(*pos + 4));
            let (quantum, offset) = (f32_at(*pos + 8), f32_at(*pos + 12));
            let start = *pos;
            *pos += 16;
            let values = read_linear(data, *pos, counts, quantum, offset, frames, chunk_size);
            *pos = start + num_ints * 4;
            let min = values.iter().cloned().fold(f32::MAX, f32::min);
            (Channel::QuantizeFloat { quantum, offset: if values.is_empty() { offset } else { min }, values }, 0)
        }
        7 | 8 => (Channel::CachedQuaternion { kind: (kind - 6) as u8, quat_index: 0 }, 0),
        _ => bail!("unknown channel type {kind}"),
    })
}

/// The chunked delta code LinearFloat uses: per chunk a start value and a bit offset to its
/// deltas, then per frame an unsigned delta of `count3` low bits plus a unary-coded high part,
/// and a sign bit when the delta is not zero.
fn read_linear(data: &[u8], at: usize, counts: i32, quantum: f32, offset: f32, frames: usize, chunk_size: usize) -> Vec<f32> {
    let (count1, count2, count3) = ((counts & 0xFF) as usize, ((counts >> 8) & 0xFF) as usize, ((counts >> 16) & 0xFF) as usize);
    let chunk = if chunk_size == 0 { 255 } else { chunk_size };
    let n_chunks = (chunk + frames - 1) / chunk;
    let start = (at * 8) as i64;
    let total = (data.len() * 8) as i64;
    let delta_base = start + (n_chunks * (count1 + count2)) as i64;
    let mut bit = start;
    let read = |n: usize, bit: &mut i64| {
        let v = get_bits(data, *bit, n);
        *bit += n as i64;
        v
    };
    let mut chunk_offsets = vec![0i64; n_chunks];
    let mut chunk_values = vec![0i32; n_chunks];
    for o in chunk_offsets.iter_mut() {
        *o = if count1 > 0 { read(count1, &mut bit) as i64 } else { 0 };
    }
    for v in chunk_values.iter_mut() {
        *v = if count2 > 0 { read(count2, &mut bit) as i32 } else { 0 };
    }
    let mut out = vec![0f32; frames];
    for i in 0..n_chunks {
        let mut value = chunk_values[i];
        let mut bit = chunk_offsets[i] + delta_base;
        let mut inc = 0i32;
        for j in 0..chunk {
            let frame = i * chunk + j;
            if frame >= frames {
                break;
            }
            out[frame] = value as f32 * quantum + offset;
            if j + 1 >= chunk {
                break;
            }
            let mut delta = if count3 != 0 { read(count3, &mut bit) as i32 } else { 0 };
            let so = bit;
            loop {
                let b = read(1, &mut bit);
                if b != 0 || bit >= total {
                    break;
                }
            }
            delta |= ((bit - so - 1) as i32) << count3;
            if delta != 0 && read(1, &mut bit) == 1 {
                delta = -delta;
            }
            inc = inc.wrapping_add(delta);
            value = value.wrapping_add(inc);
        }
    }
    out
}

fn parse_clip(rd: &ResReader, va: u64, hash: u32, hash_of: &HashMap<u64, u32>) -> Result<Clip> {
    let b = rd.resolve(va, 0x70).context("clip block out of bounds")?;
    let ty = u32_le(b, 0x10);
    let name = if u64_le(b, 0x18) != 0 { rd.string_at(u64_le(b, 0x18)).unwrap_or_default() } else { String::new() };
    let tags = if u64_le(b, 0x38) != 0 { parse_tags(rd, u64_le(b, 0x38))? } else { Vec::new() };
    let properties = if u64_le(b, 0x40) != 0 { parse_properties(rd, u64_le(b, 0x40))? } else { Vec::new() };
    let clip_animation = |at: usize| ClipAnimation {
        animation: 0,
        start: f32_le(b, at),
        end: f32_le(b, at + 4),
        rate: f32_le(b, at + 8),
    };
    // an animation's hash is its map entry's, so clips (which point at the animation block) look it up
    let animation_hash = |ptr: u64| -> Result<u32> {
        if ptr == 0 {
            return Ok(0);
        }
        hash_of.get(&ptr).copied().with_context(|| format!("a clip points at an animation (0x{ptr:X}) that is not in the animation map"))
    };
    let kind = match ty {
        1 => {
            let mut a = clip_animation(0x58);
            a.animation = animation_hash(u64_le(b, 0x50))?;
            ClipKind::Animation(a)
        }
        2 => {
            let count = u16_le(b, 0x58) as usize;
            let mut animations = Vec::new();
            if count > 0 {
                let raw = rd.resolve(u64_le(b, 0x50), count * 24).context("clip animation list out of bounds")?;
                for e in raw.chunks_exact(24) {
                    animations.push(ClipAnimation {
                        animation: animation_hash(u64_le(e, 0x10))?,
                        start: f32_le(e, 0),
                        end: f32_le(e, 4),
                        rate: f32_le(e, 8),
                    });
                }
            }
            ClipKind::List { duration: f32_le(b, 0x60), animations }
        }
        other => bail!("unknown clip type {other}"),
    };
    Ok(Clip { hash, name, unknown30: u32_le(b, 0x30), tags, properties, kind })
}

fn parse_attributes(rd: &ResReader, list_va: u64, count: usize) -> Result<Vec<Attribute>> {
    let mut out = Vec::new();
    for ptr in rd.read_u64_list(list_va, count).context("attribute pointers")? {
        let b = rd.resolve(ptr, 0x30).context("attribute out of bounds")?;
        let value = match b[8] {
            1 => AttrValue::Float(f32_le(b, 0x20)),
            2 => AttrValue::Int(u32_le(b, 0x20) as i32),
            3 => AttrValue::Bool(u32_le(b, 0x20)),
            4 => AttrValue::String(rd.string_at(u64_le(b, 0x20)).unwrap_or_default()),
            6 => AttrValue::Vector3 { value: [f32_le(b, 0x20), f32_le(b, 0x24), f32_le(b, 0x28)], unknown2c: f32_le(b, 0x2C) },
            8 => AttrValue::Vector4([f32_le(b, 0x20), f32_le(b, 0x24), f32_le(b, 0x28), f32_le(b, 0x2C)]),
            12 => AttrValue::HashString(u32_le(b, 0x20)),
            other => bail!("unknown attribute type {other}"),
        };
        out.push(Attribute { name: u32_le(b, 0x18), value });
    }
    Ok(out)
}

fn parse_property_body(rd: &ResReader, b: &[u8]) -> Result<Property> {
    Ok(Property {
        name: u32_le(b, 0x18),
        unk: u32_le(b, 0x38),
        attributes: parse_attributes(rd, u64_le(b, 0x20), u16_le(b, 0x28) as usize)?,
    })
}

fn parse_tags(rd: &ResReader, list_va: u64) -> Result<Vec<Tag>> {
    let head = rd.resolve(list_va, 0x20).context("tag list out of bounds")?;
    let count = u16_le(head, 8) as usize;
    let mut out = Vec::new();
    for ptr in rd.read_u64_list(u64_le(head, 0), count).context("tag pointers")? {
        let b = rd.resolve(ptr, 0x50).context("tag out of bounds")?;
        out.push(Tag { property: parse_property_body(rd, b)?, start_phase: f32_le(b, 0x40), end_phase: f32_le(b, 0x44) });
    }
    Ok(out)
}

fn parse_properties(rd: &ResReader, map_va: u64) -> Result<Vec<Property>> {
    let head = rd.resolve(map_va, 0x10).context("property map out of bounds")?;
    let mut out = Vec::new();
    for entry_va in chain_entries(rd, u64_le(head, 0), u16_le(head, 8) as usize)? {
        let entry = rd.resolve(entry_va, 0x20).context("property entry out of bounds")?;
        let data_va = u64_le(entry, 8);
        if data_va == 0 {
            continue;
        }
        let b = rd.resolve(data_va, 0x40).context("property out of bounds")?;
        out.push(parse_property_body(rd, b)?);
    }
    Ok(out)
}

pub use crate::ycd_write::serialize_ycd;
pub use crate::ycd_xml::{from_xml, to_xml};
