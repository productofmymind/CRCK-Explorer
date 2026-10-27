//! PSO ("PSIN"): the big-endian, self-describing container behind
//! `_manifest.ymf`, `.pso` and many `.ymt` files. A file is a sequence of
//! sections, each `ident u32, length u32` then payload:
//!
//! - `PSIN` — the data; every other section's offsets index into this one
//!   (offsets are absolute, the data section being first in the file).
//! - `PMAP` — the root block id and the block table (`name_hash, offset,
//!   unknown, length`), 1-based ids. Two header layouts exist: `root i32,
//!   count i16, 0x7070 i16` and a later one where that count reads as zero
//!   and the real `count i16` plus three more i16 follow.
//! - `PSCH` — the schema: an index of `(name_hash, offset)` then, at each
//!   offset, a structure (type 0: `type<<24 | unk<<16 | count`, `size`,
//!   `unk`, then `count × {name u32, type u8, subtype u8, offset u16,
//!   ref_key u32}`) or an enum (type 1: `type<<24 | count`, then
//!   `count × {name u32, value i32}`).
//! - `STRF`/`STRS`/`STRE`/`PSIG`/`CHKS` — string tables, signature,
//!   checksum: not needed to read the data, and skipped.
//!
//! Member types and the `subtype` byte that refines them are decoded in
//! [`walk`], which turns the whole file into a [`MetaValue`] tree.

use std::collections::HashMap;

pub use crate::meta_schema::HashSite;

use anyhow::{bail, Context, Result};

use crate::math::{Vec2, Vec3, Vec4};
use crate::value::{MetaArray, MetaDump, MetaStruct, MetaValue};

/// "PSIN", as the first four bytes of a PSO file read big-endian.
pub const PSO_MAGIC: u32 = 0x5053_494E;

const SECTION_PSIN: u32 = 0x5053_494E;
const SECTION_PMAP: u32 = 0x504D_4150;
const SECTION_PSCH: u32 = 0x5053_4348;

/// The `ARRAYINFO` pseudo-member: a schema entry that only describes the
/// element type of the array member that references it.
const ARRAYINFO: u32 = 0x100;

/// True when `data` starts with the PSO magic.
pub fn is_pso(data: &[u8]) -> bool {
    data.len() >= 4 && u32_be(data, 0) == PSO_MAGIC
}

/// PSO member types (the `type` byte of a schema entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PsoType {
    Bool = 0x00,
    SByte = 0x01,
    UByte = 0x02,
    SShort = 0x03,
    UShort = 0x04,
    SInt = 0x05,
    UInt = 0x06,
    Float = 0x07,
    Float2 = 0x08,
    Float3 = 0x09,
    Float4 = 0x0A,
    String = 0x0B,
    Structure = 0x0C,
    Array = 0x0D,
    Enum = 0x0E,
    Flags = 0x0F,
    Map = 0x10,
    Float3a = 0x14,
    Float4a = 0x15,
    HFloat = 0x1E,
    Long = 0x20,
}

impl PsoType {
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0x00 => Self::Bool,
            0x01 => Self::SByte,
            0x02 => Self::UByte,
            0x03 => Self::SShort,
            0x04 => Self::UShort,
            0x05 => Self::SInt,
            0x06 => Self::UInt,
            0x07 => Self::Float,
            0x08 => Self::Float2,
            0x09 => Self::Float3,
            0x0A => Self::Float4,
            0x0B => Self::String,
            0x0C => Self::Structure,
            0x0D => Self::Array,
            0x0E => Self::Enum,
            0x0F => Self::Flags,
            0x10 => Self::Map,
            0x14 => Self::Float3a,
            0x15 => Self::Float4a,
            0x1E => Self::HFloat,
            0x20 => Self::Long,
            _ => return None,
        })
    }
}

/// One member of a structure, as the schema describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoEntry {
    pub name_hash: u32,
    pub type_byte: u8,
    pub subtype: u8,
    pub offset: u16,
    pub ref_key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoStructInfo {
    pub name_hash: u32,
    pub size: u32,
    pub entries: Vec<PsoEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoEnumInfo {
    pub name_hash: u32,
    /// `(member name hash, value)`.
    pub entries: Vec<(u32, i32)>,
}

impl PsoEnumInfo {
    fn name_of(&self, value: i32) -> Option<u32> {
        self.entries.iter().find(|(_, v)| *v == value).map(|(n, _)| *n)
    }
}

/// A data block: a slice of the data section holding one structure
/// instance or one array's storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoBlock {
    pub name_hash: u32,
    pub offset: u32,
    pub length: u32,
}

/// A parsed PSO file: the data, the block table and the schema.
#[derive(Debug, Clone, PartialEq)]
pub struct PsoFile {
    data: Vec<u8>,
    /// 1-based id of the block holding the root structure.
    pub root_id: u32,
    pub blocks: Vec<PsoBlock>,
    pub structs: HashMap<u32, PsoStructInfo>,
    pub enums: HashMap<u32, PsoEnumInfo>,
}

pub fn u16_be(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes(b[off..off + 2].try_into().unwrap_or([0; 2]))
}
pub fn u32_be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap_or([0; 4]))
}
pub fn u64_be(b: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(b[off..off + 8].try_into().unwrap_or([0; 8]))
}
fn f32_be(b: &[u8], off: usize) -> f32 {
    f32::from_bits(u32_be(b, off))
}

/// Parses the section table and schema of a PSO file.
pub fn parse_pso(data: &[u8]) -> Result<PsoFile> {
    if !is_pso(data) {
        bail!("not a PSO file (magic {:#010x}, expected {PSO_MAGIC:#010x})", data.get(0..4).map_or(0, |b| u32_be(b, 0)));
    }
    let mut file = PsoFile { data: Vec::new(), root_id: 0, blocks: Vec::new(), structs: HashMap::new(), enums: HashMap::new() };
    let mut pos = 0usize;
    let mut saw_map = false;
    while pos + 8 <= data.len() {
        let ident = u32_be(data, pos);
        let length = u32_be(data, pos + 4) as usize;
        if length < 8 || pos + length > data.len() {
            bail!("pso: section {ident:#010x} at {pos} runs past the end of the file");
        }
        let section = &data[pos..pos + length];
        match ident {
            SECTION_PSIN => file.data = section.to_vec(),
            SECTION_PMAP => {
                parse_pmap(section, &mut file)?;
                saw_map = true;
            }
            SECTION_PSCH => parse_psch(section, &mut file)?,
            _ => {}
        }
        pos += length;
    }
    if file.data.is_empty() {
        bail!("pso: no data section");
    }
    if !saw_map {
        bail!("pso: no PMAP section");
    }
    Ok(file)
}

fn parse_pmap(section: &[u8], file: &mut PsoFile) -> Result<()> {
    if section.len() < 16 {
        bail!("pso: PMAP section too short");
    }
    file.root_id = u32_be(section, 8);
    let mut count = u16_be(section, 12) as usize;
    let mut pos = 16;
    // The later header layout: the count slot holds zero (its high bit set
    // reads as negative in a signed decoder), and the real count follows
    // with three more halfwords.
    if count == 0 || count & 0x8000 != 0 {
        if section.len() < 24 {
            bail!("pso: PMAP section too short");
        }
        count = u16_be(section, 16) as usize;
        pos = 24;
    }
    let need = pos + count * 16;
    if section.len() < need {
        bail!("pso: PMAP lists {count} blocks but is {} bytes", section.len());
    }
    for i in 0..count {
        let off = pos + i * 16;
        file.blocks.push(PsoBlock { name_hash: u32_be(section, off), offset: u32_be(section, off + 4), length: u32_be(section, off + 12) });
    }
    Ok(())
}

fn parse_psch(section: &[u8], file: &mut PsoFile) -> Result<()> {
    if section.len() < 12 {
        bail!("pso: PSCH section too short");
    }
    let count = u32_be(section, 8) as usize;
    let index_end = 12usize.checked_add(count.checked_mul(8).context("pso: schema count overflow")?).context("pso: schema count overflow")?;
    if section.len() < index_end {
        bail!("pso: PSCH lists {count} entries but is {} bytes", section.len());
    }
    for i in 0..count {
        let name_hash = u32_be(section, 12 + i * 8);
        let offset = u32_be(section, 16 + i * 8) as usize;
        let Some(head) = section.get(offset..offset + 4) else { bail!("pso: schema entry {i} offset {offset} out of bounds") };
        let word = u32_be(head, 0);
        match word >> 24 {
            0 => {
                let entries = (word & 0xFFFF) as usize;
                let size = u32_be(section, offset + 4);
                let base = offset + 12;
                let mut out = Vec::with_capacity(entries);
                for n in 0..entries {
                    let e = base + n * 12;
                    let Some(rec) = section.get(e..e + 12) else { bail!("pso: structure {name_hash:#010x} member {n} out of bounds") };
                    out.push(PsoEntry {
                        name_hash: u32_be(rec, 0),
                        type_byte: rec[4],
                        subtype: rec[5],
                        offset: u16_be(rec, 6),
                        ref_key: u32_be(rec, 8),
                    });
                }
                file.structs.entry(name_hash).or_insert(PsoStructInfo { name_hash, size, entries: out });
            }
            1 => {
                let entries = (word & 0x00FF_FFFF) as usize;
                let base = offset + 4;
                let mut out = Vec::with_capacity(entries);
                for n in 0..entries {
                    let e = base + n * 8;
                    let Some(rec) = section.get(e..e + 8) else { bail!("pso: enum {name_hash:#010x} member {n} out of bounds") };
                    out.push((u32_be(rec, 0), u32_be(rec, 4) as i32));
                }
                file.enums.entry(name_hash).or_insert(PsoEnumInfo { name_hash, entries: out });
            }
            other => bail!("pso: unknown schema entry type {other}"),
        }
    }
    Ok(())
}

impl PsoFile {
    /// The whole data section (section header included, as offsets count
    /// from the start of the file).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The block with 1-based id `id`.
    pub fn block(&self, id: u32) -> Option<&PsoBlock> {
        id.checked_sub(1).and_then(|i| self.blocks.get(i as usize))
    }

    /// The root structure's type hash.
    pub fn root_type(&self) -> Option<u32> {
        self.block(self.root_id).map(|b| b.name_hash)
    }
}

/// Decodes the whole file into a value tree, starting at the root block.
pub fn walk(file: &PsoFile) -> MetaDump {
    let mut w = Walker { file, warnings: Vec::new(), depth: 0, sites: Vec::new() };
    let root = match file.block(file.root_id) {
        Some(block) => w.read_struct(block.name_hash, block.offset as usize),
        None => {
            w.warnings.push(format!("root block {} not in the block table", file.root_id));
            MetaValue::Null
        }
    };
    MetaDump { root, warnings: w.warnings }
}

/// Parses and decodes in one go.
pub fn dump_pso(data: &[u8]) -> Result<MetaDump> {
    Ok(walk(&parse_pso(data)?))
}

/// Every hash-typed member and array element, as absolute file offsets
/// (the data section is first in the file, so its offsets are the file's).
pub fn hash_sites(file: &PsoFile) -> Vec<HashSite> {
    let mut w = Walker { file, warnings: Vec::new(), depth: 0, sites: Vec::new() };
    if let Some(block) = file.block(file.root_id) {
        w.read_struct(block.name_hash, block.offset as usize);
    }
    let mut sites: Vec<HashSite> = w.sites.into_iter().map(|(offset, value)| HashSite { offset, value }).collect();
    sites.sort_by_key(|s| s.offset);
    sites.dedup();
    sites
}

/// Rewrites every hash field equal to one of `old` to `new`, in place (a
/// PSO file's layout does not change), and returns the file with the
/// number of fields changed.
pub fn replace_hashes(data: &[u8], old: &[u32], new: u32) -> Result<(Vec<u8>, usize)> {
    let file = parse_pso(data)?;
    let mut out = data.to_vec();
    let mut changed = 0;
    for site in hash_sites(&file) {
        if old.contains(&site.value) && out.len() >= site.offset + 4 {
            out[site.offset..site.offset + 4].copy_from_slice(&new.to_be_bytes());
            changed += 1;
        }
    }
    Ok((out, changed))
}

/// A packed pointer: 1-based block id in the low 12 bits, byte offset in
/// the next 20. In a PSO file the pointer is the big-endian u32 in the first
/// four bytes of its 8-byte slot (the rest is padding), so callers pass
/// [`u32_be`] of the slot.
fn unpack_pointer(raw: u32) -> Option<(u32, usize)> {
    let block = raw & 0xFFF;
    if block == 0 {
        return None;
    }
    Some((block, ((raw >> 12) & 0xFFFFF) as usize))
}

struct Walker<'a> {
    file: &'a PsoFile,
    warnings: Vec<String>,
    depth: usize,
    /// `(absolute offset, value)` of every hash read, for [`hash_sites`].
    sites: Vec<(usize, u32)>,
}

const MAX_DEPTH: usize = 64;

impl Walker<'_> {
    fn bytes(&self, abs: usize, len: usize) -> Option<&[u8]> {
        self.file.data.get(abs..abs.checked_add(len)?)
    }

    fn warn(&mut self, msg: String) -> MetaValue {
        if self.warnings.len() < 200 {
            self.warnings.push(msg);
        }
        MetaValue::Null
    }

    /// Resolves a packed pointer to an absolute data offset.
    fn deref(&self, raw: u32) -> Option<(u32, usize)> {
        let (block_id, off) = unpack_pointer(raw)?;
        let block = self.file.block(block_id)?;
        Some((block_id, block.offset as usize + off))
    }

    fn read_struct(&mut self, type_hash: u32, abs: usize) -> MetaValue {
        if self.depth > MAX_DEPTH {
            return self.warn(format!("structure nesting deeper than {MAX_DEPTH}"));
        }
        let Some(info) = self.file.structs.get(&type_hash).cloned() else {
            return self.warn(format!("no schema for structure {type_hash:#010x}"));
        };
        self.depth += 1;
        let mut fields = Vec::with_capacity(info.entries.len());
        for (i, entry) in info.entries.iter().enumerate() {
            if entry.name_hash == ARRAYINFO {
                continue;
            }
            let value = self.read_member(&info, i, entry, abs + entry.offset as usize);
            fields.push((entry.name_hash, value));
        }
        self.depth -= 1;
        MetaValue::Struct(MetaStruct { type_hash, fields })
    }

    fn read_member(&mut self, info: &PsoStructInfo, index: usize, entry: &PsoEntry, abs: usize) -> MetaValue {
        let Some(ty) = PsoType::from_byte(entry.type_byte) else {
            return self.warn(format!("{:#010x}.{:#010x}: unknown member type {:#04x}", info.name_hash, entry.name_hash, entry.type_byte));
        };
        macro_rules! need {
            ($len:expr) => {
                match self.bytes(abs, $len) {
                    Some(b) => b,
                    None => return self.warn(format!("{:#010x}.{:#010x}: {} bytes at {abs} out of bounds", info.name_hash, entry.name_hash, $len)),
                }
            };
        }
        match ty {
            PsoType::Bool => MetaValue::Bool(need!(1)[0] != 0),
            PsoType::SByte => MetaValue::I8(need!(1)[0] as i8),
            PsoType::UByte => MetaValue::U8(need!(1)[0]),
            PsoType::SShort => MetaValue::I16(u16_be(need!(2), 0) as i16),
            PsoType::UShort => MetaValue::U16(u16_be(need!(2), 0)),
            PsoType::HFloat => MetaValue::I16(u16_be(need!(2), 0) as i16),
            PsoType::SInt => MetaValue::I32(u32_be(need!(4), 0) as i32),
            PsoType::UInt => MetaValue::U32(u32_be(need!(4), 0)),
            PsoType::Long => MetaValue::U64(u64_be(need!(8), 0)),
            PsoType::Float => MetaValue::F32(f32_be(need!(4), 0)),
            PsoType::Float2 => {
                let b = need!(8);
                MetaValue::Vec2(Vec2::new(f32_be(b, 0), f32_be(b, 4)))
            }
            PsoType::Float3 | PsoType::Float3a | PsoType::Float4a => {
                let b = need!(12);
                MetaValue::Vec3(Vec3::new(f32_be(b, 0), f32_be(b, 4), f32_be(b, 8)))
            }
            PsoType::Float4 => {
                let b = need!(16);
                MetaValue::Vec4(Vec4::new(f32_be(b, 0), f32_be(b, 4), f32_be(b, 8), f32_be(b, 12)))
            }
            PsoType::String => self.read_string(entry, abs),
            PsoType::Enum => {
                let value = match entry.subtype {
                    0 => u32_be(need!(4), 0) as i32,
                    2 => i32::from(need!(1)[0]),
                    other => return self.warn(format!("{:#010x}.{:#010x}: enum subtype {other}", info.name_hash, entry.name_hash)),
                };
                let name = self.file.enums.get(&entry.ref_key).and_then(|e| e.name_of(value));
                MetaValue::Enum { enum_hash: entry.ref_key, value, name }
            }
            PsoType::Flags => {
                let bits = match entry.subtype {
                    0 => u32_be(need!(4), 0),
                    1 => u32::from(u16_be(need!(2), 0)),
                    2 => u32::from(need!(1)[0]),
                    other => return self.warn(format!("{:#010x}.{:#010x}: flags subtype {other}", info.name_hash, entry.name_hash)),
                };
                // The enum naming the bits is referenced through an ARRAYINFO
                // entry whose index sits in the low 12 bits of `ref_key`.
                let idx = (entry.ref_key & 0xFFF) as usize;
                let enum_hash = info.entries.get(idx).filter(|e| e.name_hash == ARRAYINFO).map_or(0, |e| e.ref_key);
                let names = self
                    .file
                    .enums
                    .get(&enum_hash)
                    .map(|e| e.entries.iter().filter(|(_, v)| (0..32).contains(v) && bits & (1u32 << v) != 0).map(|(n, _)| *n).collect())
                    .unwrap_or_default();
                MetaValue::Flags { enum_hash, bits, names }
            }
            PsoType::Structure => match entry.subtype {
                0 => self.read_struct(entry.ref_key, abs),
                3 | 4 => {
                    let raw = u32_be(need!(8), 0);
                    if raw == 0 {
                        return MetaValue::Null;
                    }
                    match self.deref(raw) {
                        Some((block_id, target)) => {
                            let type_hash = self.file.block(block_id).map_or(entry.ref_key, |b| b.name_hash);
                            self.read_struct(type_hash, target)
                        }
                        None => self.warn(format!("{:#010x}.{:#010x}: dangling structure pointer", info.name_hash, entry.name_hash)),
                    }
                }
                other => self.warn(format!("{:#010x}.{:#010x}: structure subtype {other}", info.name_hash, entry.name_hash)),
            },
            PsoType::Array => self.read_array(info, index, entry, abs),
            PsoType::Map => self.read_map(info, entry, abs),
        }
    }

    fn read_string(&mut self, entry: &PsoEntry, abs: usize) -> MetaValue {
        match entry.subtype {
            0 => {
                let len = ((entry.ref_key >> 16) & 0xFFFF) as usize;
                match self.bytes(abs, len) {
                    Some(b) => MetaValue::Str(nul_terminated(b)),
                    None => self.warn(format!("string of {len} bytes at {abs} out of bounds")),
                }
            }
            1 | 2 => {
                let Some(b) = self.bytes(abs, 8) else { return self.warn(format!("string pointer at {abs} out of bounds")) };
                let raw = u32_be(b, 0);
                if raw == 0 {
                    return MetaValue::Str(String::new());
                }
                match self.deref(raw) {
                    Some((_, target)) => match self.file.data.get(target..) {
                        Some(rest) => MetaValue::Str(nul_terminated(&rest[..rest.len().min(4096)])),
                        None => self.warn(format!("string pointer at {abs} dangles")),
                    },
                    None => self.warn(format!("string pointer at {abs} dangles")),
                }
            }
            3 => {
                let Some(b) = self.bytes(abs, 16) else { return self.warn(format!("char pointer at {abs} out of bounds")) };
                let raw = u32_be(b, 0);
                let count = u16_be(b, 8) as usize;
                let cap = u16_be(b, 10) as usize;
                let len = if cap == 0 { count } else { count.min(cap) };
                if len == 0 {
                    return MetaValue::Str(String::new());
                }
                match self.deref(raw).and_then(|(_, t)| self.bytes(t, len)) {
                    Some(bytes) => MetaValue::Str(nul_terminated(bytes)),
                    None => self.warn(format!("char pointer at {abs} dangles")),
                }
            }
            7 | 8 => match self.bytes(abs, 4) {
                Some(b) => {
                    let value = u32_be(b, 0);
                    self.sites.push((abs, value));
                    MetaValue::Hash(value)
                }
                None => self.warn(format!("hash at {abs} out of bounds")),
            },
            other => self.warn(format!("string subtype {other} at {abs}")),
        }
    }

    /// The element descriptor of an array member: the ARRAYINFO entry
    /// `ref_key` indexes (low 16 bits, or 12 when that is out of range).
    fn array_info<'i>(info: &'i PsoStructInfo, entry: &PsoEntry) -> Option<&'i PsoEntry> {
        let mut idx = (entry.ref_key & 0xFFFF) as usize;
        if idx >= info.entries.len() {
            idx = (entry.ref_key & 0xFFF) as usize;
        }
        info.entries.get(idx)
    }

    fn read_array(&mut self, info: &PsoStructInfo, _index: usize, entry: &PsoEntry, abs: usize) -> MetaValue {
        let Some(elem) = Self::array_info(info, entry).cloned() else {
            return self.warn(format!("{:#010x}.{:#010x}: array without ARRAYINFO", info.name_hash, entry.name_hash));
        };
        // Where the elements are and how many: a pointer + count for
        // subtype 0 (and 4 when the elements are pointers), inline at the
        // member itself otherwise, with the count in `ref_key`'s high half.
        let (start, count) = match entry.subtype {
            0 | 4 if entry.subtype == 0 || elem.subtype == 3 => {
                let Some(b) = self.bytes(abs, 16) else { return self.warn(format!("{:#010x}.{:#010x}: array header out of bounds", info.name_hash, entry.name_hash)) };
                let raw = u32_be(b, 0);
                let count = u16_be(b, 8) as usize;
                if count == 0 {
                    return MetaValue::Array(MetaArray { item_type: Self::item_type(&elem), typed_items: elem.subtype == 3, items: Vec::new() });
                }
                match self.deref(raw) {
                    Some((_, target)) => (target, count),
                    None => return self.warn(format!("{:#010x}.{:#010x}: array pointer dangles", info.name_hash, entry.name_hash)),
                }
            }
            _ => (abs, ((entry.ref_key >> 16) & 0xFFFF) as usize),
        };
        self.read_elements(&elem, start, count)
    }

    fn item_type(elem: &PsoEntry) -> Option<u32> {
        (elem.type_byte == PsoType::Structure as u8 && elem.ref_key != 0).then_some(elem.ref_key)
    }

    fn read_elements(&mut self, elem: &PsoEntry, start: usize, count: usize) -> MetaValue {
        let Some(ty) = PsoType::from_byte(elem.type_byte) else {
            return self.warn(format!("array element type {:#04x} unknown", elem.type_byte));
        };
        let mut items = Vec::with_capacity(count.min(4096));
        let mut typed_items = false;
        let item_type = Self::item_type(elem);
        macro_rules! fixed {
            ($stride:expr, |$b:ident| $body:expr) => {{
                for n in 0..count {
                    match self.bytes(start + n * $stride, $stride) {
                        Some($b) => items.push($body),
                        None => {
                            self.warn(format!("array element {n} at {} out of bounds", start + n * $stride));
                            break;
                        }
                    }
                }
            }};
        }
        match ty {
            PsoType::Structure => match elem.subtype {
                3 => {
                    typed_items = true;
                    for n in 0..count {
                        let Some(b) = self.bytes(start + n * 8, 8) else {
                            self.warn(format!("pointer array element {n} out of bounds"));
                            break;
                        };
                        let raw = u32_be(b, 0);
                        match self.deref(raw) {
                            Some((block_id, target)) => {
                                let type_hash = self.file.block(block_id).map_or(elem.ref_key, |b| b.name_hash);
                                let v = self.read_struct(type_hash, target);
                                items.push(v);
                            }
                            None if raw == 0 => items.push(MetaValue::Null),
                            None => {
                                self.warn(format!("pointer array element {n} dangles"));
                                items.push(MetaValue::Null);
                            }
                        }
                    }
                }
                _ => {
                    let stride = self.file.structs.get(&elem.ref_key).map_or(0, |s| s.size as usize);
                    if stride == 0 {
                        return self.warn(format!("array of structure {:#010x} without a schema", elem.ref_key));
                    }
                    for n in 0..count {
                        if self.bytes(start + n * stride, stride).is_none() {
                            self.warn(format!("structure array element {n} out of bounds"));
                            break;
                        }
                        let v = self.read_struct(elem.ref_key, start + n * stride);
                        items.push(v);
                    }
                }
            },
            PsoType::String => match elem.subtype {
                7 | 8 => {
                    for n in 0..count {
                        let at = start + n * 4;
                        let Some(b) = self.bytes(at, 4) else {
                            self.warn(format!("hash array element {n} out of bounds"));
                            break;
                        };
                        let value = u32_be(b, 0);
                        self.sites.push((at, value));
                        items.push(MetaValue::Hash(value));
                    }
                }
                2 => {
                    for n in 0..count {
                        let v = self.read_string(&PsoEntry { subtype: 2, ..elem.clone() }, start + n * 8);
                        items.push(v);
                    }
                }
                3 => {
                    for n in 0..count {
                        let v = self.read_string(&PsoEntry { subtype: 3, ..elem.clone() }, start + n * 16);
                        items.push(v);
                    }
                }
                0 => {
                    let stride = ((elem.ref_key >> 16) & 0xFFFF) as usize;
                    if stride == 0 {
                        return self.warn("array of zero-length strings".into());
                    }
                    fixed!(stride, |b| MetaValue::Str(nul_terminated(b)));
                }
                other => return self.warn(format!("string array subtype {other}")),
            },
            PsoType::Array => {
                // Arrays of arrays: each element is its own 16-byte array
                // header described by the same ARRAYINFO chain.
                for n in 0..count {
                    let v = self.read_elements(elem, start + n * 16, 0);
                    items.push(v);
                }
            }
            PsoType::Bool => fixed!(1, |b| MetaValue::Bool(b[0] != 0)),
            PsoType::SByte => fixed!(1, |b| MetaValue::I8(b[0] as i8)),
            PsoType::UByte => fixed!(1, |b| MetaValue::U8(b[0])),
            PsoType::SShort => fixed!(2, |b| MetaValue::I16(u16_be(b, 0) as i16)),
            PsoType::UShort | PsoType::HFloat => fixed!(2, |b| MetaValue::U16(u16_be(b, 0))),
            PsoType::SInt => fixed!(4, |b| MetaValue::I32(u32_be(b, 0) as i32)),
            PsoType::UInt => fixed!(4, |b| MetaValue::U32(u32_be(b, 0))),
            PsoType::Long => fixed!(8, |b| MetaValue::U64(u64_be(b, 0))),
            PsoType::Float => fixed!(4, |b| MetaValue::F32(f32_be(b, 0))),
            PsoType::Float2 => fixed!(8, |b| MetaValue::Vec2(Vec2::new(f32_be(b, 0), f32_be(b, 4)))),
            // Vector3 elements are stored with vec4 alignment.
            PsoType::Float3 | PsoType::Float3a | PsoType::Float4a => fixed!(16, |b| MetaValue::Vec3(Vec3::new(f32_be(b, 0), f32_be(b, 4), f32_be(b, 8)))),
            PsoType::Float4 => fixed!(16, |b| MetaValue::Vec4(Vec4::new(f32_be(b, 0), f32_be(b, 4), f32_be(b, 8), f32_be(b, 12)))),
            PsoType::Enum => {
                let info = self.file.enums.get(&elem.ref_key).cloned();
                fixed!(4, |b| {
                    let value = u32_be(b, 0) as i32;
                    MetaValue::Enum { enum_hash: elem.ref_key, value, name: info.as_ref().and_then(|e| e.name_of(value)) }
                });
            }
            PsoType::Flags | PsoType::Map => return self.warn(format!("array of {ty:?} elements")),
        }
        MetaValue::Array(MetaArray { item_type, typed_items, items })
    }

    /// A `Map` member (subtype 1): two u32s, then an `Array_Structure` of
    /// `{key, value}` pairs whose structure the low half of `ref_key` names
    /// through an ARRAYINFO entry.
    fn read_map(&mut self, info: &PsoStructInfo, entry: &PsoEntry, abs: usize) -> MetaValue {
        if entry.subtype != 1 {
            return self.warn(format!("{:#010x}.{:#010x}: map subtype {}", info.name_hash, entry.name_hash, entry.subtype));
        }
        let Some(b) = self.bytes(abs, 24) else { return self.warn(format!("{:#010x}.{:#010x}: map header out of bounds", info.name_hash, entry.name_hash)) };
        let x1 = u32_be(b, 0);
        let (raw, count) = if x1 != 0x0100_0000 { (u32_be(b, 16), u16_be(b, 2) as usize) } else { (u32_be(b, 8), u16_be(b, 16) as usize) };
        let key_idx = (entry.ref_key & 0xFFFF) as usize;
        let Some(pair_type) = info.entries.get(key_idx).map(|e| e.ref_key) else {
            return self.warn(format!("{:#010x}.{:#010x}: map without ARRAYINFO", info.name_hash, entry.name_hash));
        };
        if count == 0 {
            return MetaValue::Map(Vec::new());
        }
        let Some((_, start)) = self.deref(raw) else { return self.warn(format!("{:#010x}.{:#010x}: map pointer dangles", info.name_hash, entry.name_hash)) };
        let stride = self.file.structs.get(&pair_type).map_or(0, |s| s.size as usize);
        if stride == 0 {
            return self.warn(format!("{:#010x}.{:#010x}: map pair structure {pair_type:#010x} unknown", info.name_hash, entry.name_hash));
        }
        let mut pairs = Vec::with_capacity(count);
        for n in 0..count {
            let MetaValue::Struct(pair) = self.read_struct(pair_type, start + n * stride) else { break };
            let mut fields = pair.fields.into_iter();
            let key = fields.next().map_or(MetaValue::Null, |(_, v)| v);
            let value = fields.next().map_or(MetaValue::Null, |(_, v)| v);
            pairs.push((key, value));
        }
        MetaValue::Map(pairs)
    }
}

fn nul_terminated(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    //! A byte-built PSO fixture: a root structure with one hash, one flags
    //! word and a pointer array of two child structures each holding a
    //! string and a float. Shared with downstream crates' tests.
    use super::*;
    use crate::hash::rage_joaat;

    fn put_u32(v: &mut [u8], at: usize, x: u32) {
        v[at..at + 4].copy_from_slice(&x.to_be_bytes());
    }
    fn put_u16(v: &mut [u8], at: usize, x: u16) {
        v[at..at + 2].copy_from_slice(&x.to_be_bytes());
    }
    fn put_u64(v: &mut [u8], at: usize, x: u64) {
        v[at..at + 8].copy_from_slice(&x.to_be_bytes());
    }
    /// A packed pointer in its 8-byte slot: the u32 first, padding after.
    fn pointer(block: u32, offset: u32) -> u64 {
        u64::from(block | (offset << 12)) << 32
    }

    pub const ROOT: &str = "TestRoot";
    pub const CHILD: &str = "TestChild";
    pub const FLAGS_ENUM: &str = "TestFlags";

    /// Builds the fixture. `new_pmap` picks the later PMAP header layout.
    pub fn sample_pso(new_pmap: bool) -> Vec<u8> {
        // Data section: header 8, root struct at 8 (size 32), child structs
        // at 40 and 64 (size 24 each), pointer array at 88 (16 bytes).
        let mut data = vec![0u8; 104];
        put_u32(&mut data, 0, SECTION_PSIN);
        put_u32(&mut data, 4, 104);
        // root: name hash @0, flags u16 @4, children Array_Structure @8.
        put_u32(&mut data, 8, rage_joaat("map1"));
        put_u16(&mut data, 12, 0b101);
        put_u64(&mut data, 16, pointer(4, 0));
        put_u16(&mut data, 24, 2);
        put_u16(&mut data, 26, 2);
        // children: label char[8] @0, weight f32 @8, kind enum u8 @12.
        data[40..46].copy_from_slice(b"first\0");
        put_u32(&mut data, 48, 1.5f32.to_bits());
        data[52] = 2;
        data[64..71].copy_from_slice(b"second\0");
        put_u32(&mut data, 72, 2.5f32.to_bits());
        data[76] = 1;
        // pointer array: two packed pointers to blocks 2 and 3.
        put_u64(&mut data, 88, pointer(2, 0));
        put_u64(&mut data, 96, pointer(3, 0));

        // PMAP: root id 1, four blocks.
        let blocks: [(u32, u32, u32); 4] = [(rage_joaat(ROOT), 8, 32), (rage_joaat(CHILD), 40, 24), (rage_joaat(CHILD), 64, 24), (7, 88, 16)];
        let head = if new_pmap { 24 } else { 16 };
        let mut pmap = vec![0u8; head + 16 * blocks.len()];
        put_u32(&mut pmap, 0, SECTION_PMAP);
        let pmap_len = pmap.len() as u32;
        put_u32(&mut pmap, 4, pmap_len);
        put_u32(&mut pmap, 8, 1);
        if new_pmap {
            put_u16(&mut pmap, 12, 0);
            put_u16(&mut pmap, 16, blocks.len() as u16);
        } else {
            put_u16(&mut pmap, 12, blocks.len() as u16);
            put_u16(&mut pmap, 14, 0x7070);
        }
        for (i, (name, off, len)) in blocks.iter().enumerate() {
            let at = head + i * 16;
            put_u32(&mut pmap, at, *name);
            put_u32(&mut pmap, at + 4, *off);
            put_u32(&mut pmap, at + 12, *len);
        }

        // PSCH: root struct, child struct, flags enum, kind enum.
        let mut body = Vec::new();
        let mut index = Vec::new();
        let mut add_struct = |name: &str, size: u32, entries: &[(u32, u8, u8, u16, u32)]| {
            index.push((rage_joaat(name), body.len()));
            body.extend_from_slice(&((entries.len() as u32) & 0xFFFF).to_be_bytes());
            body.extend_from_slice(&size.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            for (n, t, s, o, r) in entries {
                body.extend_from_slice(&n.to_be_bytes());
                body.push(*t);
                body.push(*s);
                body.extend_from_slice(&o.to_be_bytes());
                body.extend_from_slice(&r.to_be_bytes());
            }
        };
        add_struct(
            ROOT,
            32,
            &[
                (rage_joaat("name"), PsoType::String as u8, 7, 0, 0),
                (ARRAYINFO, PsoType::Enum as u8, 0, 0, rage_joaat(FLAGS_ENUM)),
                (rage_joaat("flags"), PsoType::Flags as u8, 1, 4, 1),
                (ARRAYINFO, PsoType::Structure as u8, 3, 0, rage_joaat(CHILD)),
                (rage_joaat("children"), PsoType::Array as u8, 0, 8, 3),
            ],
        );
        add_struct(
            CHILD,
            24,
            &[
                (rage_joaat("label"), PsoType::String as u8, 0, 0, 8 << 16),
                (rage_joaat("weight"), PsoType::Float as u8, 0, 8, 0),
                (rage_joaat("kind"), PsoType::Enum as u8, 2, 12, rage_joaat("TestKind")),
            ],
        );
        let mut add_enum = |name: &str, entries: &[(&str, i32)]| {
            index.push((rage_joaat(name), body.len()));
            body.extend_from_slice(&((1u32 << 24) | entries.len() as u32).to_be_bytes());
            for (n, v) in entries {
                body.extend_from_slice(&rage_joaat(n).to_be_bytes());
                body.extend_from_slice(&v.to_be_bytes());
            }
        };
        add_enum(FLAGS_ENUM, &[("FLAG_A", 0), ("FLAG_B", 1), ("FLAG_C", 2)]);
        add_enum("TestKind", &[("KIND_ONE", 1), ("KIND_TWO", 2)]);
        let index_len = 12 + 8 * index.len();
        let mut psch = Vec::with_capacity(index_len + body.len());
        psch.extend_from_slice(&SECTION_PSCH.to_be_bytes());
        psch.extend_from_slice(&((index_len + body.len()) as u32).to_be_bytes());
        psch.extend_from_slice(&(index.len() as u32).to_be_bytes());
        for (name, off) in &index {
            psch.extend_from_slice(&name.to_be_bytes());
            psch.extend_from_slice(&((index_len + off) as u32).to_be_bytes());
        }
        psch.extend_from_slice(&body);

        let mut file = data;
        file.extend_from_slice(&pmap);
        file.extend_from_slice(&psch);
        file
    }

    #[test]
    fn both_pmap_layouts_parse() {
        for new_pmap in [false, true] {
            let file = parse_pso(&sample_pso(new_pmap)).unwrap();
            assert_eq!(file.root_id, 1);
            assert_eq!(file.blocks.len(), 4);
            assert_eq!(file.root_type(), Some(rage_joaat(ROOT)));
            assert_eq!(file.structs.len(), 2);
            assert_eq!(file.enums.len(), 2);
        }
    }

    #[test]
    fn hashes_are_located_and_renamed_in_place() {
        let data = sample_pso(false);
        let file = parse_pso(&data).unwrap();
        let sites = hash_sites(&file);
        assert!(sites.iter().any(|s| s.value == rage_joaat("map1")), "{sites:?}");
        for s in &sites {
            assert_eq!(u32_be(&data, s.offset), s.value, "site {s:?} does not point at its value");
        }
        let (renamed, changed) = replace_hashes(&data, &[rage_joaat("map1")], rage_joaat("skatepark")).unwrap();
        assert_eq!(changed, 1);
        assert_eq!(renamed.len(), data.len());
        let dump = dump_pso(&renamed).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        assert_eq!(dump.root.as_struct().unwrap().field("name"), Some(&MetaValue::Hash(rage_joaat("skatepark"))));
    }

    #[test]
    fn the_tree_matches_the_bytes() {
        let dump = dump_pso(&sample_pso(false)).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let root = dump.root.as_struct().unwrap();
        assert_eq!(root.type_hash, rage_joaat(ROOT));
        assert_eq!(root.field("name"), Some(&MetaValue::Hash(rage_joaat("map1"))));
        assert_eq!(
            root.field("flags"),
            Some(&MetaValue::Flags { enum_hash: rage_joaat(FLAGS_ENUM), bits: 5, names: vec![rage_joaat("FLAG_A"), rage_joaat("FLAG_C")] })
        );
        let children = root.field("children").unwrap().as_array().unwrap();
        assert!(children.typed_items);
        assert_eq!(children.item_type, Some(rage_joaat(CHILD)));
        assert_eq!(children.items.len(), 2);
        let second = children.items[1].as_struct().unwrap();
        assert_eq!(second.field("label"), Some(&MetaValue::Str("second".into())));
        assert_eq!(second.field("weight"), Some(&MetaValue::F32(2.5)));
        assert_eq!(second.field("kind"), Some(&MetaValue::Enum { enum_hash: rage_joaat("TestKind"), value: 1, name: Some(rage_joaat("KIND_ONE")) }));
    }

    #[test]
    fn not_pso_is_refused() {
        let err = parse_pso(b"RSC7\0\0\0\0").unwrap_err().to_string();
        assert!(err.contains("not a PSO file"), "{err}");
        assert!(!is_pso(b"RSC7"));
        assert!(is_pso(b"PSIN"));
    }

    #[test]
    fn a_truncated_file_errors_instead_of_panicking() {
        let full = sample_pso(false);
        for cut in [4, 12, 40, 120, full.len() - 3] {
            let _ = parse_pso(&full[..cut]);
        }
    }
}
