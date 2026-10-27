//! Writes a [`MetaValue`] tree as an RSC7 Meta file (`.ymap`, `.ytyp`,
//! `.ymt`), the inverse of [`crate::meta_schema::dump_meta`]. The layout is
//! The reference tool's `MetaBuilder`: one data block per structure type (split
//! at 16 KB), items padded to 16 bytes, raw arrays in blocks named by their
//! element type, strings in block `0x10`, pointer arrays in block `0x07`,
//! and the root structure alone in block 1.
use anyhow::{bail, Result};

use crate::coerce;
use crate::resource::{build_rsc7_with_flags, pack_pages, rsc7_page_count, SYSTEM_BASE};
use crate::schema::{MetaEntryDef, MetaStructDef, Schema, ARRAYINFO};
use crate::value::{MetaArray, MetaStruct, MetaValue};

/// Resource version of a Meta file.
pub const META_VERSION: u32 = 2;

const BLOCK_SPLIT: usize = 0x4000;
const SOA_VECTOR: u32 = 0xE2CB_CFD4;

// Member type ids (the reference tool's MetaStructureEntryDataType).
const T_BOOL: u8 = 0x01;
const T_I8: u8 = 0x10;
const T_U8: u8 = 0x11;
const T_I16: u8 = 0x12;
const T_U16: u8 = 0x13;
const T_I32: u8 = 0x14;
const T_U32: u8 = 0x15;
const T_F32: u8 = 0x21;
const T_VEC3: u8 = 0x33;
const T_VEC4: u8 = 0x34;
const T_HASH: u8 = 0x4A;
const T_BYTE_ENUM: u8 = 0x60;
const T_INT_ENUM: u8 = 0x62;
const T_INT_FLAGS1: u8 = 0x63;
const T_SHORT_FLAGS: u8 = 0x64;
const T_INT_FLAGS2: u8 = 0x65;
const T_ARRAY: u8 = 0x52;
const T_CHAR_ARRAY: u8 = 0x40;
const T_BYTE_ARRAY: u8 = 0x50;
const T_DATA_BLOCK_PTR: u8 = 0x59;
const T_CHAR_PTR: u8 = 0x44;
const T_STRUCT_PTR: u8 = 0x07;
const T_STRUCT: u8 = 0x05;

// Blocks holding raw storage, named by what they hold.
const B_STRING: u32 = 0x10;
const B_POINTER: u32 = 0x07;
const B_HASH: u32 = 0x4A;
const B_UINT: u32 = 0x15;
const B_USHORT: u32 = 0x13;
const B_BYTE: u32 = 0x11;
const B_FLOAT: u32 = 0x21;
const B_VECTOR4: u32 = 0x33;

/// The built file and what the writer had to leave out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
}

struct Block {
    name: u32,
    data: Vec<u8>,
    root_only: bool,
}

struct Builder<'s> {
    schema: &'s Schema,
    blocks: Vec<Block>,
    used_structs: Vec<u32>,
    used_enums: Vec<u32>,
    warnings: Vec<String>,
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    put_u32(buf, off, v.to_bits());
}

fn meta_pointer(block_index: usize, offset: usize) -> u64 {
    ((block_index as u64 + 1) & 0xFFF) | ((offset as u64 & 0xF_FFFF) << 12)
}

/// Builds the file. `root` must be a structure the schema knows.
pub fn build_meta(root: &MetaValue, schema: &Schema) -> Result<Written> {
    let MetaValue::Struct(root_struct) = root else { bail!("the root of a Meta file must be a structure") };
    let Some(root_def) = schema.meta_structs.get(&root_struct.type_hash) else {
        bail!("no schema for the root structure {:#010x}; pass a file that carries it with --schema", root_struct.type_hash)
    };

    let mut b = Builder { schema, blocks: Vec::new(), used_structs: Vec::new(), used_enums: Vec::new(), warnings: Vec::new() };
    // Block 1 is the root's alone, so the root sits at its offset 0.
    b.blocks.push(Block { name: root_def.name, data: Vec::new(), root_only: true });
    let mut root_bytes = b.write_struct(root_def, Some(root_struct));
    pad16(&mut root_bytes);
    b.blocks[0].data = root_bytes;

    Ok(Written { bytes: b.assemble()?, warnings: b.warnings })
}

fn pad16(v: &mut Vec<u8>) {
    let n = v.len().next_multiple_of(16);
    v.resize(n, 0);
}

impl<'s> Builder<'s> {
    fn warn(&mut self, msg: String) {
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    fn use_struct(&mut self, name: u32) {
        if !self.used_structs.contains(&name) {
            self.used_structs.push(name);
        }
    }

    fn use_enum(&mut self, name: u32) {
        if self.schema.meta_enums.contains_key(&name) && !self.used_enums.contains(&name) {
            self.used_enums.push(name);
        }
    }

    fn ensure_block(&mut self, name: u32) -> usize {
        if let Some(i) = self.blocks.iter().position(|b| b.name == name && !b.root_only && b.data.len() < BLOCK_SPLIT) {
            return i;
        }
        self.blocks.push(Block { name, data: Vec::new(), root_only: false });
        self.blocks.len() - 1
    }

    /// Appends `bytes` (padded to 16) to the block for `name`; the pointer to them.
    fn add_item(&mut self, name: u32, bytes: &[u8]) -> u64 {
        let i = self.ensure_block(name);
        let block = &mut self.blocks[i];
        let offset = block.data.len();
        block.data.extend_from_slice(bytes);
        pad16(&mut block.data);
        meta_pointer(i, offset)
    }

    fn add_string(&mut self, s: &str) -> u64 {
        let i = self.ensure_block(B_STRING);
        let block = &mut self.blocks[i];
        let offset = block.data.len();
        block.data.extend_from_slice(s.as_bytes());
        block.data.push(0);
        meta_pointer(i, offset)
    }

    fn add_data_block(&mut self, bytes: &[u8]) -> u64 {
        self.blocks.push(Block { name: B_BYTE, data: bytes.to_vec(), root_only: false });
        meta_pointer(self.blocks.len() - 1, 0)
    }

    fn write_struct(&mut self, def: &MetaStructDef, value: Option<&MetaStruct>) -> Vec<u8> {
        self.use_struct(def.name);
        let mut buf = vec![0u8; def.size as usize];
        let mut last_arrayinfo: Option<usize> = None;
        for (i, e) in def.entries.iter().enumerate() {
            if e.name == ARRAYINFO {
                last_arrayinfo = Some(i);
                continue;
            }
            let v = value.and_then(|s| s.get(e.name));
            let elem = usize::try_from(e.ref_index).ok().and_then(|k| def.entries.get(k)).filter(|x| x.name == ARRAYINFO)
                .or_else(|| last_arrayinfo.and_then(|k| def.entries.get(k)));
            self.write_member(def, e, elem, v, &mut buf);
        }
        buf
    }

    fn write_member(&mut self, def: &MetaStructDef, e: &MetaEntryDef, elem: Option<&MetaEntryDef>, v: Option<&MetaValue>, buf: &mut Vec<u8>) {
        let off = e.offset as usize;
        let name = format!("{:#010x}.{:#010x}", def.name, e.name);
        let missing = v.is_none();
        let v = v.unwrap_or(&MetaValue::Null);
        macro_rules! need {
            ($n:expr) => {
                if off + $n > buf.len() {
                    self.warn(format!("{name}: member at {off} does not fit the {}-byte structure", buf.len()));
                    return;
                }
            };
        }
        macro_rules! bad {
            () => {{
                if !missing {
                    self.warn(format!("{name}: value {v:?} is not a {}", type_name(e.ty)));
                }
                return;
            }};
        }
        match e.ty {
            T_BOOL => { need!(1); let Some(i) = coerce::int_of(v) else { bad!() }; buf[off] = (i != 0) as u8; }
            T_I8 | T_U8 => { need!(1); let Some(i) = coerce::int_of(v) else { bad!() }; buf[off] = i as u8; }
            T_I16 | T_U16 => { need!(2); let Some(i) = coerce::int_of(v) else { bad!() }; put_u16(buf, off, i as u16); }
            T_I32 | T_U32 => { need!(4); let Some(i) = coerce::int_of(v) else { bad!() }; put_u32(buf, off, i as u32); }
            T_F32 => { need!(4); let Some(f) = coerce::float_of(v) else { bad!() }; put_f32(buf, off, f); }
            T_VEC3 => {
                need!(12);
                let Some(a) = coerce::vec_of(v, 3) else { bad!() };
                for (k, f) in a.iter().take(3).enumerate() { put_f32(buf, off + k * 4, *f); }
            }
            T_VEC4 => {
                need!(16);
                let Some(a) = coerce::vec_of(v, 4) else { bad!() };
                for (k, f) in a.iter().enumerate() { put_f32(buf, off + k * 4, *f); }
            }
            T_HASH => { need!(4); let Some(h) = coerce::hash_of(v) else { bad!() }; put_u32(buf, off, h); }
            T_BYTE_ENUM | T_INT_ENUM => {
                let size = if e.ty == T_BYTE_ENUM { 1 } else { 4 };
                need!(size);
                let members = self.schema.meta_enums.get(&e.ref_key).map(|d| d.entries.clone()).unwrap_or_default();
                let Some(x) = coerce::enum_value(v, &members) else { bad!() };
                if e.ty == T_BYTE_ENUM { buf[off] = x as u8; } else { put_u32(buf, off, x as u32); }
                if e.ty == T_INT_ENUM { self.use_enum(e.ref_key); }
            }
            T_INT_FLAGS1 | T_INT_FLAGS2 | T_SHORT_FLAGS => {
                let size = if e.ty == T_SHORT_FLAGS { 2 } else { 4 };
                need!(size);
                let members = self.schema.meta_enums.get(&e.ref_key).map(|d| d.entries.clone()).unwrap_or_default();
                let Some(bits) = coerce::flags_bits(v, &members) else { bad!() };
                if size == 2 { put_u16(buf, off, bits as u16); } else { put_u32(buf, off, bits); }
                self.use_enum(e.ref_key);
            }
            T_CHAR_ARRAY => {
                let len = e.ref_key as usize;
                need!(len);
                let Some(s) = coerce::str_of(v) else { bad!() };
                let bytes = s.as_bytes();
                let n = bytes.len().min(len.saturating_sub(1));
                buf[off..off + n].copy_from_slice(&bytes[..n]);
            }
            T_BYTE_ARRAY => {
                let count = e.ref_key as usize;
                let elem_ty = elem.map_or(T_U8, |x| x.ty);
                let stride = scalar_size(elem_ty).unwrap_or(1);
                need!(count * stride);
                match v {
                    MetaValue::Array(arr) if stride > 1 => {
                        for (k, item) in arr.items.iter().take(count).enumerate() {
                            let at = off + k * stride;
                            if elem_ty == T_F32 {
                                // a float array (CLightAttrDef.posn / dir / tangent, ...): the bits of the float, not of its integer part
                                let Some(f) = coerce::float_of(item) else { bad!() };
                                put_f32(buf, at, f);
                                continue;
                            }
                            let Some(i) = coerce::int_of(item) else { bad!() };
                            match stride { 2 => put_u16(buf, at, i as u16), _ => put_u32(buf, at, i as u32) }
                        }
                    }
                    _ => {
                        let Some(bytes) = coerce::bytes_of(v) else { bad!() };
                        let n = bytes.len().min(count * stride);
                        buf[off..off + n].copy_from_slice(&bytes[..n]);
                    }
                }
            }
            T_CHAR_PTR => {
                need!(16);
                let Some(s) = coerce::str_of(v) else { bad!() };
                if !s.is_empty() {
                    let ptr = self.add_string(&s);
                    put_u64(buf, off, ptr);
                    put_u16(buf, off + 8, s.len() as u16);
                    put_u16(buf, off + 10, s.len() as u16);
                }
            }
            T_DATA_BLOCK_PTR => {
                need!(8);
                let Some(bytes) = coerce::bytes_of(v) else { bad!() };
                if !bytes.is_empty() {
                    let ptr = self.add_data_block(&bytes);
                    put_u64(buf, off, ptr);
                }
            }
            T_STRUCT_PTR => {
                need!(8);
                match v {
                    MetaValue::Null => {}
                    MetaValue::Struct(s) => {
                        let ty = if self.schema.meta_structs.contains_key(&s.type_hash) { s.type_hash } else { e.ref_key };
                        let Some(def) = self.schema.meta_structs.get(&ty).cloned() else {
                            self.warn(format!("{name}: no schema for structure {ty:#010x}"));
                            return;
                        };
                        let bytes = self.write_struct(&def, Some(s));
                        let ptr = self.add_item(def.name, &bytes);
                        put_u64(buf, off, ptr);
                    }
                    _ => bad!(),
                }
            }
            T_STRUCT => {
                let Some(def) = self.schema.meta_structs.get(&e.ref_key).cloned() else {
                    self.warn(format!("{name}: no schema for structure {:#010x}", e.ref_key));
                    return;
                };
                need!(def.size as usize);
                let inner = match v {
                    MetaValue::Struct(s) => Some(s),
                    // An empty element (`<instancedData/>`) reads back from
                    // XML as empty text: an all-default structure.
                    MetaValue::Null => None,
                    MetaValue::Str(s) if s.is_empty() => None,
                    _ => bad!(),
                };
                let bytes = self.write_struct(&def, inner);
                buf[off..off + bytes.len()].copy_from_slice(&bytes);
            }
            T_ARRAY => {
                need!(16);
                let Some(elem) = elem.cloned() else {
                    self.warn(format!("{name}: array without ARRAYINFO in the schema"));
                    return;
                };
                let items: &[MetaValue] = match v {
                    MetaValue::Array(a) => &a.items,
                    MetaValue::Null => &[],
                    MetaValue::Str(s) if s.trim().is_empty() => &[],
                    _ => bad!(),
                };
                if items.is_empty() {
                    return;
                }
                let Some(ptr) = self.write_array(&name, &elem, items) else { return };
                put_u64(buf, off, ptr);
                put_u16(buf, off + 8, items.len() as u16);
                put_u16(buf, off + 10, items.len() as u16);
            }
            other => self.warn(format!("{name}: member type {other:#04x} is not supported")),
        }
    }

    /// Stores an array's elements and returns the pointer to them.
    fn write_array(&mut self, name: &str, elem: &MetaEntryDef, items: &[MetaValue]) -> Option<u64> {
        let mut run: Vec<u8> = Vec::new();
        macro_rules! scalars {
            ($block:expr, $size:expr, |$i:ident, $at:ident, $run:ident| $body:expr) => {{
                for (k, item) in items.iter().enumerate() {
                    let $at = k * $size;
                    let $run = &mut run;
                    $run.resize($at + $size, 0);
                    let $i = item;
                    $body
                }
                Some(self.add_item($block, &run))
            }};
        }
        match elem.ty {
            // Packed 12-byte `FloatXYZ` structures (the LOD light arrays),
            // in a block of that name as the reference tool writes them.
            T_STRUCT if elem.ref_key == SOA_VECTOR => {
                self.use_struct(SOA_VECTOR);
                scalars!(SOA_VECTOR, 12, |i, at, run| {
                    let a = coerce::vec_of(i, 3)?;
                    for (k, f) in a.iter().take(3).enumerate() { put_f32(run, at + k * 4, *f); }
                })
            }
            T_STRUCT => {
                let Some(def) = self.schema.meta_structs.get(&elem.ref_key).cloned() else {
                    self.warn(format!("{name}: no schema for element structure {:#010x}", elem.ref_key));
                    return None;
                };
                for item in items {
                    let s = match item {
                        MetaValue::Struct(s) => Some(s),
                        MetaValue::Null => None,
                        other => {
                            self.warn(format!("{name}: element {other:?} is not a structure"));
                            None
                        }
                    };
                    let bytes = self.write_struct(&def, s);
                    run.extend_from_slice(&bytes);
                }
                Some(self.add_item(def.name, &run))
            }
            T_STRUCT_PTR => {
                for item in items {
                    let ptr = match item {
                        MetaValue::Struct(s) => {
                            let ty = if self.schema.meta_structs.contains_key(&s.type_hash) { s.type_hash } else { elem.ref_key };
                            match self.schema.meta_structs.get(&ty).cloned() {
                                Some(def) => {
                                    let bytes = self.write_struct(&def, Some(s));
                                    self.add_item(def.name, &bytes)
                                }
                                None => {
                                    self.warn(format!("{name}: no schema for structure {ty:#010x}"));
                                    0
                                }
                            }
                        }
                        _ => 0,
                    };
                    run.extend_from_slice(&ptr.to_le_bytes());
                }
                Some(self.add_item(B_POINTER, &run))
            }
            T_HASH => scalars!(B_HASH, 4, |i, at, run| put_u32(run, at, coerce::hash_of(i)?)),
            T_U32 | T_I32 | T_INT_ENUM | T_INT_FLAGS1 | T_INT_FLAGS2 => scalars!(B_UINT, 4, |i, at, run| put_u32(run, at, coerce::int_of(i)? as u32)),
            T_U16 | T_I16 | T_SHORT_FLAGS => scalars!(B_USHORT, 2, |i, at, run| put_u16(run, at, coerce::int_of(i)? as u16)),
            T_U8 | T_I8 | T_BOOL | T_BYTE_ENUM => scalars!(B_BYTE, 1, |i, at, run| run[at] = coerce::int_of(i)? as u8),
            T_F32 => scalars!(B_FLOAT, 4, |i, at, run| put_f32(run, at, coerce::float_of(i)?)),
            T_VEC3 | T_VEC4 => scalars!(B_VECTOR4, 16, |i, at, run| {
                let a = coerce::vec_of(i, if elem.ty == T_VEC3 { 3 } else { 4 })?;
                for (k, f) in a.iter().enumerate() { put_f32(run, at + k * 4, *f); }
            }),
            T_CHAR_PTR => {
                for item in items {
                    let s = coerce::str_of(item).unwrap_or_default();
                    let mut hdr = [0u8; 16];
                    if !s.is_empty() {
                        let ptr = self.add_string(&s);
                        put_u64(&mut hdr, 0, ptr);
                        put_u16(&mut hdr, 8, s.len() as u16);
                        put_u16(&mut hdr, 10, s.len() as u16);
                    }
                    run.extend_from_slice(&hdr);
                }
                Some(self.add_item(T_CHAR_PTR as u32, &run))
            }
            other => {
                self.warn(format!("{name}: arrays of element type {other:#04x} are not supported"));
                None
            }
        }
    }

    /// Lays the header, schema tables and blocks out in system pages and
    /// wraps them in an RSC7 container.
    fn assemble(&mut self) -> Result<Vec<u8>> {
        let structs: Vec<MetaStructDef> = self.used_structs.iter().filter_map(|h| self.schema.meta_structs.get(h).cloned()).collect();
        let enums: Vec<crate::schema::MetaEnumDef> = self.used_enums.iter().filter_map(|h| self.schema.meta_enums.get(h).cloned()).collect();

        // Piece order: header, page map, struct table, each struct's members,
        // enum table, each enum's members, block table, data blocks.
        let mut pages = 1usize;
        let layout = loop {
            let mut sizes = vec![0x70, 16 + 8 * pages, structs.len() * 32];
            sizes.extend(structs.iter().map(|s| s.entries.len() * 16));
            sizes.push(enums.len() * 24);
            sizes.extend(enums.iter().map(|e| e.entries.len() * 8));
            sizes.push(self.blocks.len() * 16);
            sizes.extend(self.blocks.iter().map(|b| b.data.len()));
            let layout = pack_pages(&sizes, true, 128)?;
            let n = rsc7_page_count(layout.flags);
            if n == pages {
                break layout;
            }
            pages = n;
        };
        let mut sys = vec![0u8; layout.size];
        let va = |off: usize| SYSTEM_BASE + off as u64;
        let mut piece = layout.offsets.iter().copied();
        let header_off = piece.next().unwrap();
        let pagemap_off = piece.next().unwrap();
        let struct_table = piece.next().unwrap();
        let struct_members: Vec<usize> = (0..structs.len()).map(|_| piece.next().unwrap()).collect();
        let enum_table = piece.next().unwrap();
        let enum_members: Vec<usize> = (0..enums.len()).map(|_| piece.next().unwrap()).collect();
        let block_table = piece.next().unwrap();
        let block_offs: Vec<usize> = (0..self.blocks.len()).map(|_| piece.next().unwrap()).collect();

        let h = header_off;
        put_u32(&mut sys, h, 0x405b_c808);
        put_u32(&mut sys, h + 4, 1);
        put_u64(&mut sys, h + 8, va(pagemap_off));
        put_u32(&mut sys, h + 0x10, 0x5052_4430);
        put_u16(&mut sys, h + 0x14, 0x0079);
        put_u32(&mut sys, h + 0x1C, 1);
        put_u64(&mut sys, h + 0x20, if structs.is_empty() { 0 } else { va(struct_table) });
        put_u64(&mut sys, h + 0x28, if enums.is_empty() { 0 } else { va(enum_table) });
        put_u64(&mut sys, h + 0x30, va(block_table));
        put_u16(&mut sys, h + 0x48, structs.len() as u16);
        put_u16(&mut sys, h + 0x4A, enums.len() as u16);
        put_u16(&mut sys, h + 0x4C, self.blocks.len() as u16);
        sys[pagemap_off + 8] = pages as u8;

        for (i, s) in structs.iter().enumerate() {
            let r = struct_table + i * 32;
            put_u32(&mut sys, r, s.name);
            put_u32(&mut sys, r + 4, s.key);
            put_u32(&mut sys, r + 8, s.unk8);
            put_u64(&mut sys, r + 0x10, if s.entries.is_empty() { 0 } else { va(struct_members[i]) });
            put_u32(&mut sys, r + 0x18, s.size);
            put_u16(&mut sys, r + 0x1E, s.entries.len() as u16);
            for (k, e) in s.entries.iter().enumerate() {
                let m = struct_members[i] + k * 16;
                put_u32(&mut sys, m, e.name);
                put_u32(&mut sys, m + 4, e.offset);
                sys[m + 8] = e.ty;
                sys[m + 9] = e.unk9;
                put_u16(&mut sys, m + 10, e.ref_index as u16);
                put_u32(&mut sys, m + 12, e.ref_key);
            }
        }
        for (i, en) in enums.iter().enumerate() {
            let r = enum_table + i * 24;
            put_u32(&mut sys, r, en.name);
            put_u32(&mut sys, r + 4, en.key);
            put_u64(&mut sys, r + 8, if en.entries.is_empty() { 0 } else { va(enum_members[i]) });
            put_u32(&mut sys, r + 0x10, en.entries.len() as u32);
            for (k, (n, v)) in en.entries.iter().enumerate() {
                put_u32(&mut sys, enum_members[i] + k * 8, *n);
                put_u32(&mut sys, enum_members[i] + k * 8 + 4, *v as u32);
            }
        }
        for (i, b) in self.blocks.iter().enumerate() {
            let r = block_table + i * 16;
            put_u32(&mut sys, r, b.name);
            put_u32(&mut sys, r + 4, b.data.len() as u32);
            put_u64(&mut sys, r + 8, va(block_offs[i]));
            sys[block_offs[i]..block_offs[i] + b.data.len()].copy_from_slice(&b.data);
        }

        Ok(build_rsc7_with_flags(META_VERSION, layout.flags, &sys, 0, &[]))
    }
}

fn scalar_size(ty: u8) -> Option<usize> {
    Some(match ty {
        T_BOOL | T_I8 | T_U8 | T_BYTE_ENUM => 1,
        T_I16 | T_U16 | T_SHORT_FLAGS => 2,
        T_I32 | T_U32 | T_F32 | T_HASH | T_INT_ENUM | T_INT_FLAGS1 | T_INT_FLAGS2 => 4,
        _ => return None,
    })
}

fn type_name(ty: u8) -> &'static str {
    match ty {
        T_BOOL => "boolean",
        T_I8 | T_U8 | T_I16 | T_U16 | T_I32 | T_U32 => "integer",
        T_F32 => "float",
        T_VEC3 => "vector3",
        T_VEC4 => "vector4",
        T_HASH => "hash",
        T_BYTE_ENUM | T_INT_ENUM => "enum member",
        T_INT_FLAGS1 | T_INT_FLAGS2 | T_SHORT_FLAGS => "flag set",
        T_CHAR_ARRAY | T_CHAR_PTR => "string",
        T_BYTE_ARRAY | T_DATA_BLOCK_PTR => "byte array",
        T_STRUCT | T_STRUCT_PTR => "structure",
        T_ARRAY => "array",
        _ => "value",
    }
}

/// A `MetaArray` of the given items, as the writers' tests build them.
#[doc(hidden)]
pub fn array(items: Vec<MetaValue>) -> MetaValue {
    MetaValue::Array(MetaArray { item_type: None, typed_items: false, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_schema::{dump_meta, parse_meta};
    use crate::rage_joaat as j;
    use crate::value::MetaStruct;
    use crate::xml::{from_xml, to_xml};

    fn st(ty: &str, fields: Vec<(&str, MetaValue)>) -> MetaValue {
        MetaValue::Struct(MetaStruct { type_hash: j(ty), fields: fields.into_iter().map(|(n, v)| (j(n), v)).collect() })
    }

    fn sample_ymap() -> MetaValue {
        let entity = |arch: &str, x: f32| st("CEntityDef", vec![
            ("archetypeName", MetaValue::Str(arch.into())),
            ("flags", MetaValue::U32(1572864)),
            ("guid", MetaValue::U32(12345)),
            ("position", MetaValue::Vec3(crate::Vec3::new(x, 2.0, 3.0))),
            ("rotation", MetaValue::Vec4(crate::Vec4::new(0.0, 0.0, 0.0, 1.0))),
            ("scaleXY", MetaValue::F32(1.0)),
            ("scaleZ", MetaValue::F32(1.0)),
            ("parentIndex", MetaValue::I32(-1)),
            ("lodDist", MetaValue::F32(120.0)),
            ("lodLevel", MetaValue::Str("LODTYPES_DEPTH_ORPHANHD".into())),
            ("priorityLevel", MetaValue::Str("PRI_REQUIRED".into())),
        ]);
        st("CMapData", vec![
            ("name", MetaValue::Str("casas_praia_extras".into())),
            ("parent", MetaValue::Str("".into())),
            ("flags", MetaValue::U32(0)),
            ("contentFlags", MetaValue::U32(1)),
            ("streamingExtentsMin", MetaValue::Vec3(crate::Vec3::new(-10.0, -10.0, -10.0))),
            ("streamingExtentsMax", MetaValue::Vec3(crate::Vec3::new(10.0, 10.0, 10.0))),
            ("entitiesExtentsMin", MetaValue::Vec3(crate::Vec3::new(-1.0, -1.0, -1.0))),
            ("entitiesExtentsMax", MetaValue::Vec3(crate::Vec3::new(1.0, 1.0, 1.0))),
            ("entities", MetaValue::Array(MetaArray { item_type: Some(j("CEntityDef")), typed_items: true, items: vec![entity("prop_barier_conc_05b", 1.0), entity("prop_bench_01a", 5.5)] })),
            ("physicsDictionaries", array(vec![MetaValue::Str("cs1_02_phys".into())])),
            ("block", st("CBlockDesc", vec![("version", MetaValue::U32(0)), ("flags", MetaValue::U32(0)), ("name", MetaValue::Str("built by rage".into())), ("exportedBy", MetaValue::Str("rage-formats".into()))])),
        ])
    }

    #[test]
    fn a_ymap_round_trips_through_the_reader() {
        let root = sample_ymap();
        let written = build_meta(&root, Schema::builtin()).unwrap();
        assert!(written.warnings.is_empty(), "{:?}", written.warnings);
        let file = parse_meta(&written.bytes).unwrap();
        assert_eq!(file.root_type(), Some(j("CMapData")));
        let dump = dump_meta(&written.bytes).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let map = dump.root.as_struct().unwrap();
        assert_eq!(map.field("name"), Some(&MetaValue::Hash(j("casas_praia_extras"))));
        assert_eq!(map.field("contentFlags"), Some(&MetaValue::U32(1)));
        let ents = map.field("entities").unwrap().items();
        assert_eq!(ents.len(), 2);
        let e1 = ents[1].as_struct().unwrap();
        assert_eq!(e1.type_hash, j("CEntityDef"));
        assert_eq!(e1.field("archetypeName"), Some(&MetaValue::Hash(j("prop_bench_01a"))));
        assert_eq!(e1.field("position"), Some(&MetaValue::Vec3(crate::Vec3::new(5.5, 2.0, 3.0))));
        assert_eq!(e1.field("parentIndex"), Some(&MetaValue::I32(-1)));
        assert!(matches!(e1.field("lodLevel"), Some(MetaValue::Enum { name: Some(n), .. }) if *n == j("LODTYPES_DEPTH_ORPHANHD")), "{:?}", e1.field("lodLevel"));
        assert_eq!(map.field("physicsDictionaries").unwrap().items(), &[MetaValue::Hash(j("cs1_02_phys"))]);
        let block = map.field("block").unwrap().as_struct().unwrap();
        assert_eq!(block.field("exportedBy"), Some(&MetaValue::Str("rage-formats".into())));

        // The ymap reader sees the same entities.
        let ymap = crate::parse_ymap(&written.bytes).unwrap();
        assert_eq!(ymap.entities.len(), 2);
        assert_eq!(ymap.header.name_hash, j("casas_praia_extras"));
    }

    #[test]
    fn occluders_read_back_as_structures() {
        // `boxOccluders` and `occludeModels` are the two arrays whose entry
        // carries 4 in its unknown byte; their items are inline structures
        // all the same, not pointers.
        let mut root = sample_ymap();
        let MetaValue::Struct(map) = &mut root else { unreachable!() };
        let s16 = |v: i16| MetaValue::I16(v);
        let occluder = st("BoxOccluder", vec![("iCenterX", s16(40)), ("iCenterY", s16(-8)), ("iCenterZ", s16(12)), ("iCosZ", s16(0)), ("iLength", s16(16)), ("iWidth", s16(8)), ("iHeight", s16(4)), ("iSinZ", s16(0))]);
        let model = st("OccludeModel", vec![
            ("bmin", MetaValue::Vec3(crate::Vec3::new(1.0, 2.0, 3.0))),
            ("bmax", MetaValue::Vec3(crate::Vec3::new(4.0, 5.0, 6.0))),
            ("dataSize", MetaValue::U32(8)),
            ("verts", MetaValue::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8])),
            ("numVertsInBytes", MetaValue::U16(8)),
            ("numTris", MetaValue::U16(0)),
            ("flags", MetaValue::U32(0)),
        ]);
        map.fields.push((j("boxOccluders"), MetaValue::Array(MetaArray { item_type: Some(j("BoxOccluder")), typed_items: false, items: vec![occluder.clone(), occluder] })));
        map.fields.push((j("occludeModels"), MetaValue::Array(MetaArray { item_type: Some(j("OccludeModel")), typed_items: false, items: vec![model] })));
        let written = build_meta(&root, Schema::builtin()).unwrap();
        assert!(written.warnings.is_empty(), "{:?}", written.warnings);
        let dump = dump_meta(&written.bytes).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let map = dump.root.as_struct().unwrap();
        let boxes = map.field("boxOccluders").unwrap().items();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].as_struct().unwrap().field("iCenterX").and_then(MetaValue::as_i64), Some(40));
        let models = map.field("occludeModels").unwrap().items();
        let m = models[0].as_struct().unwrap();
        assert_eq!(m.field("bmax"), Some(&MetaValue::Vec3(crate::Vec3::new(4.0, 5.0, 6.0))));
        assert_eq!(m.field("verts"), Some(&MetaValue::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8])));
    }

    #[test]
    fn distant_light_positions_are_packed_twelve_bytes_apart() {
        let mut root = sample_ymap();
        let MetaValue::Struct(map) = &mut root else { unreachable!() };
        let positions: Vec<MetaValue> = (0..3).map(|i| MetaValue::Vec3(crate::Vec3::new(i as f32, 10.0 + i as f32, 20.0 + i as f32))).collect();
        map.fields.push((j("DistantLODLightsSOA"), st("CDistantLODLight", vec![("position", array(positions.clone())), ("RGBI", array(vec![MetaValue::U32(1); 3])), ("numStreetLights", MetaValue::U16(0)), ("category", MetaValue::U16(0))])));
        let written = build_meta(&root, Schema::builtin()).unwrap();
        assert!(written.warnings.is_empty(), "{:?}", written.warnings);
        let file = parse_meta(&written.bytes).unwrap();
        let block = file.blocks.iter().find(|b| b.name_hash == SOA_VECTOR).expect("a FloatXYZ block");
        let f = |at: usize| f32::from_le_bytes(block.data[at..at + 4].try_into().unwrap());
        assert_eq!((f(12), f(16), f(20)), (1.0, 11.0, 21.0));
        let dump = dump_meta(&written.bytes).unwrap();
        let soa = dump.root.as_struct().unwrap().field("DistantLODLightsSOA").unwrap().as_struct().unwrap();
        assert_eq!(soa.field("position").unwrap().items(), positions.as_slice());
    }

    #[test]
    fn an_empty_element_writes_an_empty_structure_quietly() {
        let text = "<CMapData><name>m</name><instancedData/></CMapData>";
        let written = build_meta(&from_xml(text).unwrap(), Schema::builtin()).unwrap();
        assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    }

    #[test]
    fn dumped_xml_builds_back_to_the_same_dump() {
        let written = build_meta(&sample_ymap(), Schema::builtin()).unwrap();
        let dump1 = dump_meta(&written.bytes).unwrap();
        for names in [crate::names::NameTable::default(), crate::names::NameTable::core()] {
        let xml = to_xml(&dump1.root, &names);
        let value = from_xml(&xml).unwrap();
        let again = build_meta(&value, Schema::builtin()).unwrap();
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
        let dump2 = dump_meta(&again.bytes).unwrap();
        assert_eq!(to_xml(&dump2.root, &names), xml);
        }
    }

    #[test]
    fn the_files_own_schema_is_enough_to_rebuild_it() {
        let written = build_meta(&sample_ymap(), Schema::builtin()).unwrap();
        let own = Schema::from_file(&written.bytes).unwrap();
        assert!(own.meta_structs.contains_key(&j("CMapData")) && own.meta_structs.contains_key(&j("CEntityDef")));
        assert_eq!(own.meta_structs[&j("CMapData")], Schema::builtin().meta_structs[&j("CMapData")]);
        let dump = dump_meta(&written.bytes).unwrap();
        let again = build_meta(&dump.root, &own).unwrap();
        assert_eq!(dump_meta(&again.bytes).unwrap().root, dump.root);
    }

    #[test]
    fn unknown_root_and_bad_values_are_reported() {
        let err = build_meta(&st("NotAThing", vec![]), Schema::builtin()).unwrap_err();
        assert!(err.to_string().contains("no schema for the root"), "{err}");
        let root = st("CMapData", vec![("flags", MetaValue::Str("lots".into())), ("entities", MetaValue::Str("no".into()))]);
        let w = build_meta(&root, Schema::builtin()).unwrap();
        assert_eq!(w.warnings.len(), 2, "{:?}", w.warnings);
        assert!(w.warnings[0].contains("is not a integer") || w.warnings[0].contains("not a"), "{:?}", w.warnings);
    }
}
