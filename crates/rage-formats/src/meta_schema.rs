//! The self-describing side of the RSC7 "Meta" container (`.ytyp`, `.ymap`,
//! Meta-form `.ymt`). Besides the data blocks that `ytyp.rs` and `ymap.rs`
//! read by fixed offsets, the Meta header points at a table of structure
//! infos (name hash, size, members with offset and type) and enum infos,
//! which is enough to decode any block into a [`MetaValue`] tree without
//! knowing the file's structures in advance.
//!
//! Header (112 bytes at `SYSTEM_BASE`): root block index (1-based) at
//! 0x1C, `StructureInfosPointer` 0x20, `EnumInfosPointer` 0x28,
//! `DataBlocksPointer` 0x30, and the three `i16` counts at 0x48, 0x4A,
//! 0x4C. A structure info is 32 bytes (`name u32, key u32, unk, unk,
//! entries ptr u64, size i32, unk i16, count i16`), each member 16 bytes
//! (`name u32, offset i32, type u8, unk u8, ref index i16, ref key u32`); an
//! enum info is 24 bytes (`name u32, key u32, entries ptr u64, count i32,
//! unk`), each member `name u32, value i32`.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::hash::rage_joaat;
use crate::math::{Vec3, Vec4};
use crate::resource::{f32_le, prepare_rsc7, u16_le, u32_le, u64_le, ResReader, SYSTEM_BASE};
use crate::value::{MetaArray, MetaDump, MetaStruct, MetaValue};
use crate::ytyp::{decode_meta_pointer, read_meta_blocks, MetaBlock};

/// The `ARRAYINFO` pseudo-member describing an array member's elements.
const ARRAYINFO: u32 = 0x100;

/// The element type of `CLODLight`'s structure-of-arrays vector members
/// (`direction` and friends): a vec4-aligned vector the file's schema never
/// defines, so it is decoded here as a `Vec3` with a 16-byte stride.
const SOA_VECTOR: u32 = 0xE2CB_CFD4;

/// Meta member types (`MetaStructureEntryDataType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaType {
    Boolean = 0x01,
    SignedByte = 0x10,
    UnsignedByte = 0x11,
    SignedShort = 0x12,
    UnsignedShort = 0x13,
    SignedInt = 0x14,
    UnsignedInt = 0x15,
    Float = 0x21,
    Vec3 = 0x33,
    Vec4 = 0x34,
    ByteEnum = 0x60,
    IntEnum = 0x62,
    IntFlags1 = 0x63,
    ShortFlags = 0x64,
    IntFlags2 = 0x65,
    Hash = 0x4A,
    Array = 0x52,
    CharArray = 0x40,
    ByteArray = 0x50,
    DataBlockPointer = 0x59,
    CharPointer = 0x44,
    StructurePointer = 0x07,
    Structure = 0x05,
}

impl MetaType {
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => Self::Boolean,
            0x10 => Self::SignedByte,
            0x11 => Self::UnsignedByte,
            0x12 => Self::SignedShort,
            0x13 => Self::UnsignedShort,
            0x14 => Self::SignedInt,
            0x15 => Self::UnsignedInt,
            0x21 => Self::Float,
            0x33 => Self::Vec3,
            0x34 => Self::Vec4,
            0x60 => Self::ByteEnum,
            0x62 => Self::IntEnum,
            0x63 => Self::IntFlags1,
            0x64 => Self::ShortFlags,
            0x65 => Self::IntFlags2,
            0x4A => Self::Hash,
            0x52 => Self::Array,
            0x40 => Self::CharArray,
            0x50 => Self::ByteArray,
            0x59 => Self::DataBlockPointer,
            0x44 => Self::CharPointer,
            0x07 => Self::StructurePointer,
            0x05 => Self::Structure,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaEntryInfo {
    pub name_hash: u32,
    pub offset: i32,
    pub type_byte: u8,
    pub unknown: u8,
    /// For an array member, the index of its `ARRAYINFO` entry.
    pub ref_index: i16,
    /// A structure or enum name hash, or a length, depending on the type.
    pub ref_key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaStructInfo {
    pub name_hash: u32,
    pub size: i32,
    pub entries: Vec<MetaEntryInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaEnumInfo {
    pub name_hash: u32,
    pub entries: Vec<(u32, i32)>,
}

/// The schema a Meta file carries, plus its data blocks.
pub struct MetaFile {
    pub root_block: usize,
    pub structs: HashMap<u32, MetaStructInfo>,
    pub enums: HashMap<u32, MetaEnumInfo>,
    pub(crate) blocks: Vec<MetaBlock>,
}

impl MetaFile {
    /// The root structure's type hash.
    pub fn root_type(&self) -> Option<u32> {
        self.blocks.get(self.root_block).map(|b| b.name_hash)
    }

    /// `(name hash, length)` of every data block.
    pub fn block_summary(&self) -> Vec<(u32, usize)> {
        self.blocks.iter().map(|b| (b.name_hash, b.data.len())).collect()
    }
}

/// Reads the schema and data blocks of an RSC7 Meta file.
pub fn parse_meta(data: &[u8]) -> Result<MetaFile> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    parse_meta_from_reader(&reader)
}

pub(crate) fn parse_meta_from_reader(reader: &ResReader<'_>) -> Result<MetaFile> {
    let header = reader.resolve(SYSTEM_BASE, 0x70).context("meta: header out of bounds")?;
    let root_index = u32_le(header, 0x1C) as usize;
    let struct_ptr = u64_le(header, 0x20);
    let enum_ptr = u64_le(header, 0x28);
    let struct_count = u16_le(header, 0x48) as usize;
    let enum_count = u16_le(header, 0x4A) as usize;

    let mut structs = HashMap::with_capacity(struct_count);
    if struct_count > 0 {
        let table = reader.resolve(struct_ptr, struct_count * 32).context("meta: structure info table out of bounds")?;
        for i in 0..struct_count {
            let rec = &table[i * 32..i * 32 + 32];
            let name_hash = u32_le(rec, 0);
            let entries_ptr = u64_le(rec, 0x10);
            let size = u32_le(rec, 0x18) as i32;
            let count = u16_le(rec, 0x1E) as usize;
            let mut entries = Vec::with_capacity(count);
            if count > 0 {
                let bytes = reader.resolve(entries_ptr, count * 16).with_context(|| format!("meta: members of structure {name_hash:#010x} out of bounds"))?;
                for n in 0..count {
                    let e = &bytes[n * 16..n * 16 + 16];
                    entries.push(MetaEntryInfo {
                        name_hash: u32_le(e, 0),
                        offset: u32_le(e, 4) as i32,
                        type_byte: e[8],
                        unknown: e[9],
                        ref_index: u16_le(e, 10) as i16,
                        ref_key: u32_le(e, 12),
                    });
                }
            }
            structs.entry(name_hash).or_insert(MetaStructInfo { name_hash, size, entries });
        }
    }

    let mut enums = HashMap::with_capacity(enum_count);
    if enum_count > 0 {
        let table = reader.resolve(enum_ptr, enum_count * 24).context("meta: enum info table out of bounds")?;
        for i in 0..enum_count {
            let rec = &table[i * 24..i * 24 + 24];
            let name_hash = u32_le(rec, 0);
            let entries_ptr = u64_le(rec, 8);
            let count = u32_le(rec, 0x10) as usize;
            let mut entries = Vec::with_capacity(count);
            if count > 0 {
                let bytes = reader.resolve(entries_ptr, count * 8).with_context(|| format!("meta: members of enum {name_hash:#010x} out of bounds"))?;
                for n in 0..count {
                    entries.push((u32_le(bytes, n * 8), u32_le(bytes, n * 8 + 4) as i32));
                }
            }
            enums.entry(name_hash).or_insert(MetaEnumInfo { name_hash, entries });
        }
    }

    let blocks = read_meta_blocks(reader)?;
    if root_index == 0 || root_index > blocks.len() {
        bail!("meta: root block index {root_index} out of range (1..={})", blocks.len());
    }
    Ok(MetaFile { root_block: root_index - 1, structs, enums, blocks })
}

/// Decodes the whole file from its root block.
pub fn walk(file: &MetaFile) -> MetaDump {
    let mut w = Walker { file, warnings: Vec::new(), depth: 0, sites: Vec::new() };
    let root = match file.blocks.get(file.root_block) {
        Some(block) => w.read_struct(block.name_hash, file.root_block, 0),
        None => {
            w.warnings.push("root block missing".into());
            MetaValue::Null
        }
    };
    MetaDump { root, warnings: w.warnings }
}

/// Parses and decodes in one go.
pub fn dump_meta(data: &[u8]) -> Result<MetaDump> {
    Ok(walk(&parse_meta(data)?))
}

/// Where a hash-typed value sits in a file: the byte offset (into the
/// inflated system section for Meta, into the file for PSO) and what is
/// there now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashSite {
    pub offset: usize,
    pub value: u32,
}

/// Every hash-typed member and array element the schema describes, as
/// offsets into the inflated system section. Padding, strings and numbers
/// are never listed, so a rename touches only what is a name.
pub fn hash_sites(file: &MetaFile) -> Vec<HashSite> {
    let mut w = Walker { file, warnings: Vec::new(), depth: 0, sites: Vec::new() };
    if let Some(block) = file.blocks.get(file.root_block) {
        w.read_struct(block.name_hash, file.root_block, 0);
    }
    let mut sites: Vec<HashSite> = w
        .sites
        .into_iter()
        .filter_map(|(block, off, value)| {
            let base = file.blocks.get(block)?.system_offset?;
            Some(HashSite { offset: base + off, value })
        })
        .collect();
    sites.sort_by_key(|s| s.offset);
    sites.dedup();
    sites
}

/// Rewrites every hash field equal to one of `old` to `new` and returns
/// the RSC7 file again (same version, sections re-paged) with the number
/// of fields changed. The file's bytes are otherwise untouched.
pub fn replace_hashes(data: &[u8], old: &[u32], new: u32) -> Result<(Vec<u8>, usize)> {
    let file = parse_meta(data)?;
    let sites = hash_sites(&file);
    let (mut system, graphics) = prepare_rsc7(data)?;
    let version = crate::resource::resource_version_from_flags(u32_le(data, 8), u32_le(data, 12));
    let mut changed = 0;
    for site in sites {
        if old.contains(&site.value) && system.len() >= site.offset + 4 {
            system[site.offset..site.offset + 4].copy_from_slice(&new.to_le_bytes());
            changed += 1;
        }
    }
    Ok((crate::resource::build_rsc7(version, &system, &graphics), changed))
}

const MAX_DEPTH: usize = 64;

struct Walker<'a> {
    file: &'a MetaFile,
    warnings: Vec<String>,
    depth: usize,
    /// `(block, offset, value)` of every hash read, for [`hash_sites`].
    sites: Vec<(usize, usize, u32)>,
}

impl Walker<'_> {
    fn warn(&mut self, msg: String) -> MetaValue {
        if self.warnings.len() < 200 {
            self.warnings.push(msg);
        }
        MetaValue::Null
    }

    fn bytes(&self, block: usize, off: usize, len: usize) -> Option<&[u8]> {
        self.file.blocks.get(block)?.data.get(off..off.checked_add(len)?)
    }

    fn read_struct(&mut self, type_hash: u32, block: usize, off: usize) -> MetaValue {
        if self.depth > MAX_DEPTH {
            return self.warn(format!("structure nesting deeper than {MAX_DEPTH}"));
        }
        let Some(info) = self.file.structs.get(&type_hash).cloned() else {
            return self.warn(format!("no schema for structure {type_hash:#010x}"));
        };
        self.depth += 1;
        let mut fields = Vec::with_capacity(info.entries.len());
        let mut last_arrayinfo: Option<usize> = None;
        for (i, entry) in info.entries.iter().enumerate() {
            if entry.name_hash == ARRAYINFO {
                last_arrayinfo = Some(i);
                continue;
            }
            let abs = off.wrapping_add(entry.offset as usize);
            let value = self.read_member(&info, entry, last_arrayinfo, block, abs);
            fields.push((entry.name_hash, value));
        }
        self.depth -= 1;
        MetaValue::Struct(MetaStruct { type_hash, fields })
    }

    fn read_member(&mut self, info: &MetaStructInfo, entry: &MetaEntryInfo, last_arrayinfo: Option<usize>, block: usize, abs: usize) -> MetaValue {
        let Some(ty) = MetaType::from_byte(entry.type_byte) else {
            return self.warn(format!("{:#010x}.{:#010x}: unknown member type {:#04x}", info.name_hash, entry.name_hash, entry.type_byte));
        };
        macro_rules! need {
            ($len:expr) => {
                match self.bytes(block, abs, $len) {
                    Some(b) => b,
                    None => return self.warn(format!("{:#010x}.{:#010x}: {} bytes at block {block} offset {abs} out of bounds", info.name_hash, entry.name_hash, $len)),
                }
            };
        }
        match ty {
            MetaType::Boolean => MetaValue::Bool(need!(1)[0] != 0),
            MetaType::SignedByte => MetaValue::I8(need!(1)[0] as i8),
            MetaType::UnsignedByte => MetaValue::U8(need!(1)[0]),
            MetaType::SignedShort => MetaValue::I16(u16_le(need!(2), 0) as i16),
            MetaType::UnsignedShort => MetaValue::U16(u16_le(need!(2), 0)),
            MetaType::SignedInt => MetaValue::I32(u32_le(need!(4), 0) as i32),
            MetaType::UnsignedInt => MetaValue::U32(u32_le(need!(4), 0)),
            MetaType::Float => MetaValue::F32(f32_le(need!(4), 0)),
            MetaType::Vec3 => {
                let b = need!(12);
                MetaValue::Vec3(Vec3::new(f32_le(b, 0), f32_le(b, 4), f32_le(b, 8)))
            }
            MetaType::Vec4 => {
                let b = need!(16);
                MetaValue::Vec4(Vec4::new(f32_le(b, 0), f32_le(b, 4), f32_le(b, 8), f32_le(b, 12)))
            }
            MetaType::Hash => {
                let value = u32_le(need!(4), 0);
                self.sites.push((block, abs, value));
                MetaValue::Hash(value)
            }
            MetaType::ByteEnum => {
                let value = i32::from(need!(1)[0]);
                MetaValue::Enum { enum_hash: entry.ref_key, value, name: self.enum_name(entry.ref_key, value) }
            }
            MetaType::IntEnum => {
                let value = u32_le(need!(4), 0) as i32;
                MetaValue::Enum { enum_hash: entry.ref_key, value, name: self.enum_name(entry.ref_key, value) }
            }
            MetaType::IntFlags1 | MetaType::IntFlags2 => self.flags(entry.ref_key, u32_le(need!(4), 0)),
            MetaType::ShortFlags => self.flags(entry.ref_key, u32::from(u16_le(need!(2), 0))),
            MetaType::CharArray => MetaValue::Str(nul_terminated(need!(entry.ref_key as usize))),
            MetaType::CharPointer => {
                let b = need!(16);
                let count = u16_le(b, 8) as usize;
                let cap = u16_le(b, 10) as usize;
                let len = if cap == 0 { count } else { count.min(cap) };
                if len == 0 {
                    return MetaValue::Str(String::new());
                }
                match decode_meta_pointer(u64_le(b, 0)).and_then(|(bi, bo)| self.bytes(bi, bo, len)) {
                    Some(s) => MetaValue::Str(nul_terminated(s)),
                    None => self.warn(format!("{:#010x}.{:#010x}: string pointer dangles", info.name_hash, entry.name_hash)),
                }
            }
            MetaType::DataBlockPointer => {
                let raw = u64_le(need!(8), 0);
                match decode_meta_pointer(raw) {
                    None => MetaValue::Bytes(Vec::new()),
                    Some((bi, _)) => match self.file.blocks.get(bi) {
                        Some(b) => MetaValue::Bytes(b.data.clone()),
                        None => self.warn(format!("{:#010x}.{:#010x}: data block pointer dangles", info.name_hash, entry.name_hash)),
                    },
                }
            }
            MetaType::Structure => self.read_struct(entry.ref_key, block, abs),
            MetaType::StructurePointer => {
                let raw = u64_le(need!(8), 0);
                match decode_meta_pointer(raw) {
                    None => MetaValue::Null,
                    Some((bi, bo)) => match self.file.blocks.get(bi).map(|b| b.name_hash) {
                        Some(type_hash) => self.read_struct(type_hash, bi, bo),
                        None => self.warn(format!("{:#010x}.{:#010x}: structure pointer dangles", info.name_hash, entry.name_hash)),
                    },
                }
            }
            MetaType::ByteArray => {
                let elem = self.element_info(info, entry, last_arrayinfo);
                let count = entry.ref_key as usize;
                let elem_ty = elem.and_then(|e| MetaType::from_byte(e.type_byte)).unwrap_or(MetaType::UnsignedByte);
                match elem_ty {
                    MetaType::UnsignedByte | MetaType::SignedByte | MetaType::Boolean => MetaValue::Bytes(need!(count).to_vec()),
                    _ => self.read_elements(elem_ty, 0, block, abs, count),
                }
            }
            MetaType::Array => {
                let Some(elem) = self.element_info(info, entry, last_arrayinfo).cloned() else {
                    return self.warn(format!("{:#010x}.{:#010x}: array without ARRAYINFO", info.name_hash, entry.name_hash));
                };
                let b = need!(16);
                let raw = u64_le(b, 0);
                let count = u16_le(b, 8) as usize;
                // The element type alone says whether items are pointers:
                // `boxOccluders`/`occludeModels` carry 4 in the entry's
                // unknown byte and still hold inline structures.
                let pointer_items = elem.type_byte == MetaType::StructurePointer as u8;
                let item_type = ((elem.type_byte == MetaType::Structure as u8 || pointer_items) && elem.ref_key != 0 && elem.ref_key != SOA_VECTOR).then_some(elem.ref_key);
                let Some((bi, bo)) = decode_meta_pointer(raw) else {
                    return MetaValue::Array(MetaArray { item_type, typed_items: pointer_items, items: Vec::new() });
                };
                if pointer_items {
                    let mut items = Vec::with_capacity(count.min(4096));
                    for n in 0..count {
                        let Some(p) = self.bytes(bi, bo + n * 8, 8) else {
                            self.warn(format!("{:#010x}.{:#010x}: pointer array element {n} out of bounds", info.name_hash, entry.name_hash));
                            break;
                        };
                        match decode_meta_pointer(u64_le(p, 0)) {
                            None => items.push(MetaValue::Null),
                            Some((ei, eo)) => match self.file.blocks.get(ei).map(|b| b.name_hash) {
                                Some(type_hash) => {
                                    let v = self.read_struct(type_hash, ei, eo);
                                    items.push(v);
                                }
                                None => {
                                    self.warn(format!("{:#010x}.{:#010x}: pointer array element {n} dangles", info.name_hash, entry.name_hash));
                                    items.push(MetaValue::Null);
                                }
                            },
                        }
                    }
                    return MetaValue::Array(MetaArray { item_type, typed_items: true, items });
                }
                let Some(elem_ty) = MetaType::from_byte(elem.type_byte) else {
                    return self.warn(format!("{:#010x}.{:#010x}: array element type {:#04x} unknown", info.name_hash, entry.name_hash, elem.type_byte));
                };
                self.read_elements(elem_ty, elem.ref_key, bi, bo, count)
            }
        }
    }

    /// The `ARRAYINFO` describing an array member's elements: the one its
    /// `ref_index` names when that is an `ARRAYINFO`, else the one seen
    /// last.
    fn element_info<'i>(&self, info: &'i MetaStructInfo, entry: &MetaEntryInfo, last: Option<usize>) -> Option<&'i MetaEntryInfo> {
        let by_index = usize::try_from(entry.ref_index).ok().and_then(|i| info.entries.get(i)).filter(|e| e.name_hash == ARRAYINFO);
        by_index.or_else(|| last.and_then(|i| info.entries.get(i)))
    }

    /// Reads `count` inline elements of type `ty` starting at block/offset,
    /// continuing into the next block when the elements are structures
    /// laid out across block boundaries.
    fn read_elements(&mut self, ty: MetaType, ref_key: u32, block: usize, off: usize, count: usize) -> MetaValue {
        let mut items = Vec::with_capacity(count.min(4096));
        let item_type = (ty == MetaType::Structure && ref_key != SOA_VECTOR).then_some(ref_key);
        let stride = match ty {
            MetaType::Boolean | MetaType::SignedByte | MetaType::UnsignedByte | MetaType::ByteEnum => 1,
            MetaType::SignedShort | MetaType::UnsignedShort | MetaType::ShortFlags => 2,
            MetaType::SignedInt | MetaType::UnsignedInt | MetaType::Float | MetaType::Hash | MetaType::IntEnum | MetaType::IntFlags1 | MetaType::IntFlags2 => 4,
            // Vector3 elements are stored with vec4 alignment.
            MetaType::Vec3 | MetaType::Vec4 | MetaType::CharPointer | MetaType::Array => 16,
            MetaType::StructurePointer | MetaType::DataBlockPointer => 8,
            // `FloatXYZ`: three packed floats.
            MetaType::Structure if ref_key == SOA_VECTOR => 12,
            MetaType::Structure => self.file.structs.get(&ref_key).map_or(0, |s| s.size.max(0) as usize),
            MetaType::CharArray | MetaType::ByteArray => ref_key.max(1) as usize,
        };
        if stride == 0 {
            return self.warn(format!("array of structure {ref_key:#010x} without a schema"));
        }
        let (mut bi, mut bo) = (block, off);
        for n in 0..count {
            // Structure arrays may run on into the following block.
            if ty == MetaType::Structure && n > 0 && self.file.blocks.get(bi).is_some_and(|b| bo >= b.data.len()) {
                bi += 1;
                bo = 0;
            }
            let Some(b) = self.bytes(bi, bo, stride) else {
                self.warn(format!("array element {n} at block {bi} offset {bo} out of bounds"));
                break;
            };
            let v = match ty {
                MetaType::Boolean => MetaValue::Bool(b[0] != 0),
                MetaType::SignedByte => MetaValue::I8(b[0] as i8),
                MetaType::UnsignedByte => MetaValue::U8(b[0]),
                MetaType::SignedShort => MetaValue::I16(u16_le(b, 0) as i16),
                MetaType::UnsignedShort => MetaValue::U16(u16_le(b, 0)),
                MetaType::SignedInt => MetaValue::I32(u32_le(b, 0) as i32),
                MetaType::UnsignedInt => MetaValue::U32(u32_le(b, 0)),
                MetaType::Float => MetaValue::F32(f32_le(b, 0)),
                MetaType::Hash => {
                    let value = u32_le(b, 0);
                    self.sites.push((bi, bo, value));
                    MetaValue::Hash(value)
                }
                MetaType::Vec3 => MetaValue::Vec3(Vec3::new(f32_le(b, 0), f32_le(b, 4), f32_le(b, 8))),
                MetaType::Vec4 => MetaValue::Vec4(Vec4::new(f32_le(b, 0), f32_le(b, 4), f32_le(b, 8), f32_le(b, 12))),
                MetaType::ByteEnum => {
                    let value = i32::from(b[0]);
                    MetaValue::Enum { enum_hash: ref_key, value, name: self.enum_name(ref_key, value) }
                }
                MetaType::IntEnum => {
                    let value = u32_le(b, 0) as i32;
                    MetaValue::Enum { enum_hash: ref_key, value, name: self.enum_name(ref_key, value) }
                }
                MetaType::IntFlags1 | MetaType::IntFlags2 => self.flags(ref_key, u32_le(b, 0)),
                MetaType::ShortFlags => self.flags(ref_key, u32::from(u16_le(b, 0))),
                MetaType::CharArray => MetaValue::Str(nul_terminated(b)),
                MetaType::ByteArray => MetaValue::Bytes(b.to_vec()),
                MetaType::Structure if ref_key == SOA_VECTOR => MetaValue::Vec3(Vec3::new(f32_le(b, 0), f32_le(b, 4), f32_le(b, 8))),
                MetaType::Structure => self.read_struct(ref_key, bi, bo),
                MetaType::CharPointer => {
                    let count = u16_le(b, 8) as usize;
                    match decode_meta_pointer(u64_le(b, 0)).and_then(|(sbi, sbo)| self.bytes(sbi, sbo, count)) {
                        Some(s) => MetaValue::Str(nul_terminated(s)),
                        None if count == 0 => MetaValue::Str(String::new()),
                        None => self.warn(format!("string array element {n} dangles")),
                    }
                }
                MetaType::StructurePointer => match decode_meta_pointer(u64_le(b, 0)) {
                    None => MetaValue::Null,
                    Some((ei, eo)) => match self.file.blocks.get(ei).map(|blk| blk.name_hash) {
                        Some(type_hash) => self.read_struct(type_hash, ei, eo),
                        None => self.warn(format!("pointer array element {n} dangles")),
                    },
                },
                MetaType::DataBlockPointer => match decode_meta_pointer(u64_le(b, 0)).and_then(|(ei, _)| self.file.blocks.get(ei)) {
                    Some(blk) => MetaValue::Bytes(blk.data.clone()),
                    None => MetaValue::Bytes(Vec::new()),
                },
                MetaType::Array => {
                    // Nested arrays share the element descriptor chain; the
                    // inner element type is not recorded, so keep the bytes.
                    MetaValue::Bytes(b.to_vec())
                }
            };
            items.push(v);
            bo += stride;
        }
        MetaValue::Array(MetaArray { item_type, typed_items: false, items })
    }

    fn enum_name(&self, enum_hash: u32, value: i32) -> Option<u32> {
        self.file.enums.get(&enum_hash)?.entries.iter().find(|(_, v)| *v == value).map(|(n, _)| *n)
    }

    fn flags(&self, enum_hash: u32, bits: u32) -> MetaValue {
        let names = self
            .file
            .enums
            .get(&enum_hash)
            .map(|e| e.entries.iter().filter(|(_, v)| (0..32).contains(v) && bits & (1u32 << v) != 0).map(|(n, _)| *n).collect())
            .unwrap_or_default();
        MetaValue::Flags { enum_hash, bits, names }
    }
}

fn nul_terminated(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Hashes of the structure names most map files use, for callers that
/// dispatch on [`MetaFile::root_type`].
pub mod known {
    use super::rage_joaat;
    pub fn cmapdata() -> u32 {
        rage_joaat("CMapData")
    }
    pub fn cmaptypes() -> u32 {
        rage_joaat("CMapTypes")
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    //! A byte-built Meta fixture carrying its schema: a root `TestMap` with
    //! a hash, a flags word, a vec3, a string and a pointer array of two
    //! `TestEntity` structures. Shared with downstream crates' tests.
    use super::*;
    use crate::resource::build_rsc7;

    fn put_u16(s: &mut [u8], off: usize, v: u16) {
        s[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u32(s: &mut [u8], off: usize, v: u32) {
        s[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u64(s: &mut [u8], off: usize, v: u64) {
        s[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn put_f32(s: &mut [u8], off: usize, v: f32) {
        s[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn packed(block_index: usize, offset: usize) -> u64 {
        (block_index as u64 + 1) | ((offset as u64) << 12)
    }

    pub const ROOT: &str = "TestMap";
    pub const ENTITY: &str = "TestEntity";
    pub const FLAGS_ENUM: &str = "TestMapFlags";

    /// The Meta system section: header, info tables, then data blocks.
    pub fn sample_meta_system() -> Vec<u8> {
        let mut sys = vec![0u8; 0x600];
        // Layout: structure infos at 0x70 (2 × 32), entries at 0xB0
        // (root 7 × 16 = 0x70, entity 3 × 16 = 0x30), enum info at 0x150
        // (24), enum entries at 0x170 (2 × 8), block headers at 0x180
        // (4 × 16), data at 0x200.
        let (root_entries, entity_entries, enum_info, enum_entries, block_headers) = (0xB0usize, 0x120usize, 0x150usize, 0x170usize, 0x180usize);
        let (root_data, string_data, ptr_data, entity_data) = (0x200usize, 0x260usize, 0x270usize, 0x280usize);
        let root_size = 0x50u32;
        let entity_size = 0x20u32;

        put_u32(&mut sys, 0x1C, 1); // root block index
        put_u64(&mut sys, 0x20, SYSTEM_BASE + 0x70);
        put_u64(&mut sys, 0x28, SYSTEM_BASE + enum_info as u64);
        put_u64(&mut sys, 0x30, SYSTEM_BASE + block_headers as u64);
        put_u16(&mut sys, 0x48, 2);
        put_u16(&mut sys, 0x4A, 1);
        put_u16(&mut sys, 0x4C, 4);

        let mut struct_info = |at: usize, name: &str, entries_at: usize, size: u32, count: u16| {
            put_u32(&mut sys, at, rage_joaat(name));
            put_u64(&mut sys, at + 0x10, SYSTEM_BASE + entries_at as u64);
            put_u32(&mut sys, at + 0x18, size);
            put_u16(&mut sys, at + 0x1E, count);
        };
        struct_info(0x70, ROOT, root_entries, root_size, 7);
        struct_info(0x90, ENTITY, entity_entries, entity_size, 3);

        let mut entry = |at: usize, name: u32, offset: u32, ty: MetaType, unknown: u8, ref_index: i16, ref_key: u32| {
            put_u32(&mut sys, at, name);
            put_u32(&mut sys, at + 4, offset);
            sys[at + 8] = ty as u8;
            sys[at + 9] = unknown;
            put_u16(&mut sys, at + 10, ref_index as u16);
            put_u32(&mut sys, at + 12, ref_key);
        };
        // root: name Hash @8, flags IntFlags1 @12, origin Vec3 @16,
        // label CharPointer @32, ARRAYINFO, entities Array(ptr) @48, count UInt @64.
        entry(root_entries, rage_joaat("name"), 8, MetaType::Hash, 0, 0, 0);
        entry(root_entries + 16, rage_joaat("flags"), 12, MetaType::IntFlags1, 0, 0, rage_joaat(FLAGS_ENUM));
        entry(root_entries + 32, rage_joaat("origin"), 16, MetaType::Vec3, 0, 0, 0);
        entry(root_entries + 48, rage_joaat("label"), 32, MetaType::CharPointer, 0, 0, 0);
        entry(root_entries + 64, ARRAYINFO, 0, MetaType::StructurePointer, 0, 0, rage_joaat(ENTITY));
        entry(root_entries + 80, rage_joaat("entities"), 48, MetaType::Array, 0, 4, 0);
        entry(root_entries + 96, rage_joaat("count"), 64, MetaType::UnsignedInt, 0, 0, 0);
        // entity: archetype Hash @0, position Vec3 @16, kind ByteEnum @4.
        entry(entity_entries, rage_joaat("archetype"), 0, MetaType::Hash, 0, 0, 0);
        entry(entity_entries + 16, rage_joaat("kind"), 4, MetaType::ByteEnum, 0, 0, rage_joaat(FLAGS_ENUM));
        entry(entity_entries + 32, rage_joaat("position"), 16, MetaType::Vec3, 0, 0, 0);

        put_u32(&mut sys, enum_info, rage_joaat(FLAGS_ENUM));
        put_u64(&mut sys, enum_info + 8, SYSTEM_BASE + enum_entries as u64);
        put_u32(&mut sys, enum_info + 0x10, 2);
        put_u32(&mut sys, enum_entries, rage_joaat("FLAG_LOW"));
        put_u32(&mut sys, enum_entries + 4, 0);
        put_u32(&mut sys, enum_entries + 8, rage_joaat("FLAG_HIGH"));
        put_u32(&mut sys, enum_entries + 12, 3);

        let blocks: [(u32, usize, usize); 4] = [
            (rage_joaat(ROOT), root_data, root_size as usize),
            (0x10, string_data, 16),
            (0x07, ptr_data, 16),
            (rage_joaat(ENTITY), entity_data, 2 * entity_size as usize),
        ];
        for (i, (hash, off, len)) in blocks.iter().enumerate() {
            let h = block_headers + i * 16;
            put_u32(&mut sys, h, *hash);
            put_u32(&mut sys, h + 4, *len as u32);
            put_u64(&mut sys, h + 8, SYSTEM_BASE + *off as u64);
        }

        put_u32(&mut sys, root_data + 8, rage_joaat("map1"));
        put_u32(&mut sys, root_data + 12, 0b1001);
        put_f32(&mut sys, root_data + 16, 1.0);
        put_f32(&mut sys, root_data + 20, 2.0);
        put_f32(&mut sys, root_data + 24, 3.0);
        put_u64(&mut sys, root_data + 32, packed(1, 0));
        put_u16(&mut sys, root_data + 40, 5);
        put_u64(&mut sys, root_data + 48, packed(2, 0));
        put_u16(&mut sys, root_data + 56, 2);
        put_u32(&mut sys, root_data + 64, 42);
        sys[string_data..string_data + 6].copy_from_slice(b"hello\0");
        put_u64(&mut sys, ptr_data, packed(3, 0));
        put_u64(&mut sys, ptr_data + 8, packed(3, entity_size as usize));
        for (n, (name, x)) in [("prop_a", 10.0f32), ("prop_b", 20.0)].iter().enumerate() {
            let e = entity_data + n * entity_size as usize;
            put_u32(&mut sys, e, rage_joaat(name));
            sys[e + 4] = 3;
            put_f32(&mut sys, e + 16, *x);
        }
        sys
    }

    pub fn sample_meta() -> Vec<u8> {
        build_rsc7(2, &sample_meta_system(), &[])
    }

    #[test]
    fn schema_tables_parse() {
        let file = parse_meta(&sample_meta()).unwrap();
        assert_eq!(file.root_type(), Some(rage_joaat(ROOT)));
        assert_eq!(file.structs[&rage_joaat(ROOT)].entries.len(), 7);
        assert_eq!(file.structs[&rage_joaat(ENTITY)].size, 0x20);
        assert_eq!(file.enums[&rage_joaat(FLAGS_ENUM)].entries, vec![(rage_joaat("FLAG_LOW"), 0), (rage_joaat("FLAG_HIGH"), 3)]);
    }

    #[test]
    fn hashes_are_located_and_renamed_in_place() {
        let data = sample_meta();
        let file = parse_meta(&data).unwrap();
        let sites = hash_sites(&file);
        assert!(sites.iter().any(|s| s.value == rage_joaat("map1")), "{sites:?}");
        assert!(sites.iter().any(|s| s.value == rage_joaat("prop_b")), "{sites:?}");
        let (system, _) = prepare_rsc7(&data).unwrap();
        for s in &sites {
            assert_eq!(u32_le(&system, s.offset), s.value, "site {s:?} does not point at its value");
        }
        let (renamed, changed) = replace_hashes(&data, &[rage_joaat("map1")], rage_joaat("casas_praia_extras")).unwrap();
        assert_eq!(changed, 1);
        let dump = dump_meta(&renamed).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let root = dump.root.as_struct().unwrap();
        assert_eq!(root.field("name"), Some(&MetaValue::Hash(rage_joaat("casas_praia_extras"))));
        assert_eq!(root.field("label"), Some(&MetaValue::Str("hello".into())));
        assert_eq!(replace_hashes(&data, &[0xDEAD_BEEF], 1).unwrap().1, 0);
    }

    #[test]
    fn the_tree_matches_the_bytes() {
        let dump = dump_meta(&sample_meta()).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let root = dump.root.as_struct().unwrap();
        assert_eq!(root.field("name"), Some(&MetaValue::Hash(rage_joaat("map1"))));
        assert_eq!(root.field("flags"), Some(&MetaValue::Flags { enum_hash: rage_joaat(FLAGS_ENUM), bits: 9, names: vec![rage_joaat("FLAG_LOW"), rage_joaat("FLAG_HIGH")] }));
        assert_eq!(root.field("origin"), Some(&MetaValue::Vec3(Vec3::new(1.0, 2.0, 3.0))));
        assert_eq!(root.field("label"), Some(&MetaValue::Str("hello".into())));
        assert_eq!(root.field("count"), Some(&MetaValue::U32(42)));
        let entities = root.field("entities").unwrap().as_array().unwrap();
        assert!(entities.typed_items);
        assert_eq!(entities.item_type, Some(rage_joaat(ENTITY)));
        let b = entities.items[1].as_struct().unwrap();
        assert_eq!(b.type_hash, rage_joaat(ENTITY));
        assert_eq!(b.field("archetype"), Some(&MetaValue::Hash(rage_joaat("prop_b"))));
        assert_eq!(b.field("position"), Some(&MetaValue::Vec3(Vec3::new(20.0, 0.0, 0.0))));
        assert_eq!(b.field("kind"), Some(&MetaValue::Enum { enum_hash: rage_joaat(FLAGS_ENUM), value: 3, name: Some(rage_joaat("FLAG_HIGH")) }));
    }

    #[test]
    fn a_file_without_schema_reports_it_instead_of_panicking() {
        let mut sys = sample_meta_system();
        put_u16(&mut sys, 0x48, 0); // no structure infos
        let dump = dump_meta(&build_rsc7(2, &sys, &[])).unwrap();
        assert_eq!(dump.root, MetaValue::Null);
        assert!(dump.warnings[0].contains("no schema"), "{:?}", dump.warnings);
    }
}
