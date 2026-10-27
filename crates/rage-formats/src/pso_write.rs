//! Writes a [`MetaValue`] tree as a PSO file (`_manifest.ymf`, `.pso`, the
//! PSO form of `.ymt`), the inverse of [`crate::pso::dump_pso`]. Big-endian
//! throughout, laid out as the reference tool's `PsoBuilder` does: one data block
//! per structure type, raw storage in blocks named by what they hold, the
//! root structure added last, then the PMAP block table and the PSCH
//! schema of every structure and enum written.
use anyhow::{bail, Result};

use crate::coerce;
use crate::meta_write::Written;
use crate::schema::{PsoEntryDef, PsoStructDef, Schema};
use crate::value::{MetaStruct, MetaValue};

const MAX_BLOCK: usize = 0x10_0000;

// Member type ids (the reference tool's PsoDataType).
const T_BOOL: u8 = 0x00;
const T_I8: u8 = 0x01;
const T_U8: u8 = 0x02;
const T_I16: u8 = 0x03;
const T_U16: u8 = 0x04;
const T_I32: u8 = 0x05;
const T_U32: u8 = 0x06;
const T_F32: u8 = 0x07;
const T_VEC2: u8 = 0x08;
const T_VEC3: u8 = 0x09;
const T_VEC4: u8 = 0x0A;
const T_STRING: u8 = 0x0B;
const T_STRUCT: u8 = 0x0C;
const T_ARRAY: u8 = 0x0D;
const T_ENUM: u8 = 0x0E;
const T_FLAGS: u8 = 0x0F;
const T_MAP: u8 = 0x10;
const T_VEC3A: u8 = 0x14;
const T_VEC4A: u8 = 0x15;
const T_HFLOAT: u8 = 0x1E;
const T_LONG: u8 = 0x20;

// Blocks holding raw storage (the reference tool's PsoBuilder).
const B_STRING: u32 = 1;
const B_BYTE: u32 = 2;
const B_SHORT: u32 = 4;
const B_INT: u32 = 5;
const B_UINT: u32 = 6;
const B_FLOAT: u32 = 7;
const B_POINTER: u32 = 12;
const B_CHAR_POINTER: u32 = 200;

const SECTION_PSIN: u32 = 0x5053_494E;
const SECTION_PMAP: u32 = 0x504D_4150;
const SECTION_PSCH: u32 = 0x5053_4348;

struct Block {
    name: u32,
    data: Vec<u8>,
}

struct Builder<'s> {
    schema: &'s Schema,
    blocks: Vec<Block>,
    used_structs: Vec<u32>,
    used_enums: Vec<u32>,
    warnings: Vec<String>,
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_be_bytes());
}
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_be_bytes());
}
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_be_bytes());
}
fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    put_u32(buf, off, v.to_bits());
}

/// A packed pointer: 1-based block id in the low 12 bits, offset above.
fn pso_pointer(block_index: usize, offset: usize) -> u32 {
    ((block_index as u32 + 1) & 0xFFF) | ((offset as u32) << 12)
}

/// Builds the file. `root` must be a structure the schema knows.
pub fn build_pso(root: &MetaValue, schema: &Schema) -> Result<Written> {
    let MetaValue::Struct(root_struct) = root else { bail!("the root of a PSO file must be a structure") };
    let Some(root_def) = schema.pso_structs.get(&root_struct.type_hash).cloned() else {
        bail!("no PSO schema for the root structure {:#010x}; pass a file that carries it with --schema", root_struct.type_hash)
    };
    let mut b = Builder { schema, blocks: Vec::new(), used_structs: Vec::new(), used_enums: Vec::new(), warnings: Vec::new() };
    let bytes = b.write_struct(&root_def, Some(root_struct));
    let root_ptr = b.add_item(root_def.name, &bytes);
    let root_id = root_ptr & 0xFFF;
    Ok(Written { bytes: b.assemble(root_id), warnings: b.warnings })
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
        if self.schema.pso_enums.contains_key(&name) && !self.used_enums.contains(&name) {
            self.used_enums.push(name);
        }
    }

    fn ensure_block(&mut self, name: u32, need: usize) -> usize {
        if let Some(i) = self.blocks.iter().position(|b| b.name == name && b.data.len() + need <= MAX_BLOCK) {
            return i;
        }
        self.blocks.push(Block { name, data: Vec::new() });
        self.blocks.len() - 1
    }

    fn add_item(&mut self, name: u32, bytes: &[u8]) -> u32 {
        let i = self.ensure_block(name, bytes.len());
        let block = &mut self.blocks[i];
        let offset = block.data.len();
        block.data.extend_from_slice(bytes);
        pso_pointer(i, offset)
    }

    fn add_string(&mut self, s: &str) -> u32 {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        self.add_item(B_STRING, &bytes)
    }

    fn enum_of(&self, hash: u32) -> Vec<(u32, i32)> {
        self.schema.pso_enums.get(&hash).map(|e| e.entries.clone()).unwrap_or_default()
    }

    fn array_info<'d>(def: &'d PsoStructDef, e: &PsoEntryDef) -> Option<&'d PsoEntryDef> {
        let mut idx = (e.ref_key & 0xFFFF) as usize;
        if idx >= def.entries.len() {
            idx = (e.ref_key & 0xFFF) as usize;
        }
        def.entries.get(idx)
    }

    fn write_struct(&mut self, def: &PsoStructDef, value: Option<&MetaStruct>) -> Vec<u8> {
        self.use_struct(def.name);
        let mut buf = vec![0u8; def.size as usize];
        for e in &def.entries {
            if e.name == crate::schema::ARRAYINFO {
                continue;
            }
            let v = value.and_then(|s| s.get(e.name));
            self.write_member(def, e, v, &mut buf);
        }
        buf
    }

    fn write_member(&mut self, def: &PsoStructDef, e: &PsoEntryDef, v: Option<&MetaValue>, buf: &mut Vec<u8>) {
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
            T_BOOL | T_I8 | T_U8 => { need!(1); let Some(i) = coerce::int_of(v) else { bad!() }; buf[off] = i as u8; }
            T_I16 | T_U16 | T_HFLOAT => { need!(2); let Some(i) = coerce::int_of(v) else { bad!() }; put_u16(buf, off, i as u16); }
            T_I32 | T_U32 => { need!(4); let Some(i) = coerce::int_of(v) else { bad!() }; put_u32(buf, off, i as u32); }
            T_LONG => { need!(8); let Some(i) = coerce::int_of(v) else { bad!() }; put_u64(buf, off, i as u64); }
            T_F32 => { need!(4); let Some(f) = coerce::float_of(v) else { bad!() }; put_f32(buf, off, f); }
            T_VEC2 | T_VEC3 | T_VEC4 | T_VEC3A | T_VEC4A => {
                let n = match e.ty { T_VEC2 => 2, T_VEC3 | T_VEC3A => 3, _ => 4 };
                need!(n * 4);
                let Some(a) = coerce::vec_of(v, n) else { bad!() };
                for (k, f) in a.iter().take(n).enumerate() { put_f32(buf, off + k * 4, *f); }
            }
            T_STRING => match e.subtype {
                0 => {
                    let len = ((e.ref_key >> 16) & 0xFFFF) as usize;
                    need!(len);
                    let Some(s) = coerce::str_of(v) else { bad!() };
                    let n = s.len().min(len.saturating_sub(1));
                    buf[off..off + n].copy_from_slice(&s.as_bytes()[..n]);
                }
                1 | 2 => {
                    need!(8);
                    let Some(s) = coerce::str_of(v) else { bad!() };
                    if !s.is_empty() {
                        let ptr = self.add_string(&s);
                        put_u32(buf, off, ptr);
                    }
                }
                3 => {
                    need!(16);
                    let Some(s) = coerce::str_of(v) else { bad!() };
                    if !s.is_empty() {
                        let ptr = self.add_string(&s);
                        put_u32(buf, off, ptr);
                        put_u16(buf, off + 8, s.len() as u16);
                        put_u16(buf, off + 10, s.len() as u16);
                    }
                }
                7 | 8 => { need!(4); let Some(h) = coerce::hash_of(v) else { bad!() }; put_u32(buf, off, h); }
                other => self.warn(format!("{name}: string subtype {other} is not supported")),
            },
            T_ENUM => {
                let size = match e.subtype { 0 => 4, 1 => 2, _ => 1 };
                need!(size);
                let members = self.enum_of(e.ref_key);
                let Some(x) = coerce::enum_value(v, &members) else { bad!() };
                match size { 4 => put_u32(buf, off, x as u32), 2 => put_u16(buf, off, x as u16), _ => buf[off] = x as u8 }
                self.use_enum(e.ref_key);
            }
            T_FLAGS => {
                let size = match e.subtype { 0 => 4, 1 => 2, _ => 1 };
                need!(size);
                let enum_hash = def.entries.get((e.ref_key & 0xFFF) as usize).map_or(0, |a| a.ref_key);
                let members = self.enum_of(enum_hash);
                let Some(bits) = coerce::flags_bits(v, &members) else { bad!() };
                match size { 4 => put_u32(buf, off, bits), 2 => put_u16(buf, off, bits as u16), _ => buf[off] = bits as u8 }
                self.use_enum(enum_hash);
            }
            T_STRUCT => {
                let inner = match v {
                    MetaValue::Struct(s) => Some(s),
                    // An empty element (`<instancedData/>`) reads back from
                    // XML as empty text: an all-default structure.
                    MetaValue::Null => None,
                    MetaValue::Str(s) if s.is_empty() => None,
                    _ => bad!(),
                };
                if e.subtype == 0 {
                    let Some(sdef) = self.schema.pso_structs.get(&e.ref_key).cloned() else {
                        self.warn(format!("{name}: no schema for structure {:#010x}", e.ref_key));
                        return;
                    };
                    need!(sdef.size as usize);
                    let bytes = self.write_struct(&sdef, inner);
                    buf[off..off + bytes.len()].copy_from_slice(&bytes);
                } else {
                    need!(8);
                    let Some(s) = inner else { return };
                    let ty = if self.schema.pso_structs.contains_key(&s.type_hash) { s.type_hash } else { e.ref_key };
                    let Some(sdef) = self.schema.pso_structs.get(&ty).cloned() else {
                        self.warn(format!("{name}: no schema for structure {ty:#010x}"));
                        return;
                    };
                    let bytes = self.write_struct(&sdef, Some(s));
                    let ptr = self.add_item(sdef.name, &bytes);
                    put_u32(buf, off, ptr);
                }
            }
            T_ARRAY => {
                let Some(elem) = Self::array_info(def, e).cloned() else {
                    self.warn(format!("{name}: array without ARRAYINFO in the schema"));
                    return;
                };
                let items: &[MetaValue] = match v {
                    MetaValue::Array(a) => &a.items,
                    MetaValue::Null => &[],
                    MetaValue::Str(s) if s.trim().is_empty() => &[],
                    _ => bad!(),
                };
                let by_pointer = e.subtype == 0 || (e.subtype == 4 && elem.subtype == 3);
                if by_pointer {
                    need!(16);
                    if items.is_empty() {
                        return;
                    }
                    let Some((run, block)) = self.element_run(&name, &elem, items) else { return };
                    let ptr = self.add_item(block, &run);
                    put_u32(buf, off, ptr);
                    put_u16(buf, off + 8, items.len() as u16);
                    put_u16(buf, off + 10, items.len() as u16);
                } else {
                    // Fixed-size array inline at the member.
                    let count = ((e.ref_key >> 16) & 0xFFFF) as usize;
                    let Some((run, _)) = self.element_run(&name, &elem, &items[..items.len().min(count)]) else { return };
                    need!(run.len());
                    buf[off..off + run.len()].copy_from_slice(&run);
                }
            }
            T_MAP => {
                if e.subtype != 1 {
                    self.warn(format!("{name}: map subtype {} is not supported", e.subtype));
                    return;
                }
                need!(24);
                let pairs: &[(MetaValue, MetaValue)] = match v {
                    MetaValue::Map(p) => p,
                    MetaValue::Null => &[],
                    _ => bad!(),
                };
                put_u32(buf, off, 0x0100_0000);
                if pairs.is_empty() {
                    return;
                }
                let pair_type = def.entries.get((e.ref_key & 0xFFFF) as usize).map_or(0, |a| a.ref_key);
                let Some(pdef) = self.schema.pso_structs.get(&pair_type).cloned() else {
                    self.warn(format!("{name}: no schema for map pair structure {pair_type:#010x}"));
                    return;
                };
                let mut run = Vec::new();
                for (k, val) in pairs {
                    let fields: Vec<(u32, MetaValue)> = pdef.entries.iter().filter(|x| x.name != crate::schema::ARRAYINFO).zip([k, val]).map(|(x, v)| (x.name, v.clone())).collect();
                    let s = MetaStruct { type_hash: pair_type, fields };
                    let bytes = self.write_struct(&pdef, Some(&s));
                    run.extend_from_slice(&bytes);
                }
                let ptr = self.add_item(pair_type, &run);
                put_u32(buf, off + 8, ptr);
                put_u16(buf, off + 16, pairs.len() as u16);
                put_u16(buf, off + 18, pairs.len() as u16);
            }
            other => self.warn(format!("{name}: member type {other:#04x} is not supported")),
        }
    }

    /// The bytes of an array's elements and the block they belong in.
    fn element_run(&mut self, name: &str, elem: &PsoEntryDef, items: &[MetaValue]) -> Option<(Vec<u8>, u32)> {
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
                Some((run, $block))
            }};
        }
        match elem.ty {
            T_STRUCT if elem.subtype == 3 => {
                for item in items {
                    let mut slot = [0u8; 8];
                    if let MetaValue::Struct(s) = item {
                        let ty = if self.schema.pso_structs.contains_key(&s.type_hash) { s.type_hash } else { elem.ref_key };
                        match self.schema.pso_structs.get(&ty).cloned() {
                            Some(sdef) => {
                                let bytes = self.write_struct(&sdef, Some(s));
                                let ptr = self.add_item(sdef.name, &bytes);
                                put_u32(&mut slot, 0, ptr);
                            }
                            None => self.warn(format!("{name}: no schema for structure {ty:#010x}")),
                        }
                    }
                    run.extend_from_slice(&slot);
                }
                Some((run, B_POINTER))
            }
            T_STRUCT => {
                let Some(sdef) = self.schema.pso_structs.get(&elem.ref_key).cloned() else {
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
                    let bytes = self.write_struct(&sdef, s);
                    run.extend_from_slice(&bytes);
                }
                Some((run, sdef.name))
            }
            T_STRING => match elem.subtype {
                7 | 8 => scalars!(B_UINT, 4, |i, at, run| put_u32(run, at, coerce::hash_of(i)?)),
                1 | 2 => {
                    for item in items {
                        let mut slot = [0u8; 8];
                        let s = coerce::str_of(item).unwrap_or_default();
                        if !s.is_empty() {
                            let ptr = self.add_string(&s);
                            put_u32(&mut slot, 0, ptr);
                        }
                        run.extend_from_slice(&slot);
                    }
                    Some((run, B_POINTER))
                }
                3 => {
                    for item in items {
                        let mut hdr = [0u8; 16];
                        let s = coerce::str_of(item).unwrap_or_default();
                        if !s.is_empty() {
                            let ptr = self.add_string(&s);
                            put_u32(&mut hdr, 0, ptr);
                            put_u16(&mut hdr, 8, s.len() as u16);
                            put_u16(&mut hdr, 10, s.len() as u16);
                        }
                        run.extend_from_slice(&hdr);
                    }
                    Some((run, B_CHAR_POINTER))
                }
                0 => {
                    let stride = ((elem.ref_key >> 16) & 0xFFFF) as usize;
                    if stride == 0 {
                        self.warn(format!("{name}: array of zero-length strings"));
                        return None;
                    }
                    scalars!(B_STRING, stride, |i, at, run| {
                        let s = coerce::str_of(i)?;
                        let n = s.len().min(stride - 1);
                        run[at..at + n].copy_from_slice(&s.as_bytes()[..n]);
                    })
                }
                other => {
                    self.warn(format!("{name}: string array subtype {other} is not supported"));
                    None
                }
            },
            T_BOOL | T_U8 | T_I8 => scalars!(B_BYTE, 1, |i, at, run| run[at] = coerce::int_of(i)? as u8),
            T_U16 | T_I16 | T_HFLOAT => scalars!(B_SHORT, 2, |i, at, run| put_u16(run, at, coerce::int_of(i)? as u16)),
            T_I32 => scalars!(B_INT, 4, |i, at, run| put_u32(run, at, coerce::int_of(i)? as u32)),
            T_U32 => scalars!(B_UINT, 4, |i, at, run| put_u32(run, at, coerce::int_of(i)? as u32)),
            T_LONG => scalars!(B_UINT, 8, |i, at, run| put_u64(run, at, coerce::int_of(i)? as u64)),
            T_F32 => scalars!(B_FLOAT, 4, |i, at, run| put_f32(run, at, coerce::float_of(i)?)),
            T_VEC2 => scalars!(B_STRING, 8, |i, at, run| {
                let a = coerce::vec_of(i, 2)?;
                for (k, f) in a.iter().take(2).enumerate() { put_f32(run, at + k * 4, *f); }
            }),
            T_VEC3 | T_VEC3A | T_VEC4 | T_VEC4A => scalars!(B_STRING, 16, |i, at, run| {
                let n = if matches!(elem.ty, T_VEC4 | T_VEC4A) { 4 } else { 3 };
                let a = coerce::vec_of(i, n)?;
                for (k, f) in a.iter().take(n).enumerate() { put_f32(run, at + k * 4, *f); }
            }),
            T_ENUM => {
                let members = self.enum_of(elem.ref_key);
                self.use_enum(elem.ref_key);
                scalars!(B_INT, 4, |i, at, run| put_u32(run, at, coerce::enum_value(i, &members)? as u32))
            }
            other => {
                self.warn(format!("{name}: arrays of element type {other:#04x} are not supported"));
                None
            }
        }
    }

    fn assemble(&mut self, root_id: u32) -> Vec<u8> {
        // PSIN: header, 8 reserved bytes, then the blocks back to back.
        let mut psin = vec![0u8; 16];
        let mut offsets = Vec::with_capacity(self.blocks.len());
        for b in &self.blocks {
            offsets.push(psin.len());
            psin.extend_from_slice(&b.data);
        }
        let psin_len = psin.len() as u32;
        put_u32(&mut psin, 0, SECTION_PSIN);
        put_u32(&mut psin, 4, psin_len);

        let mut pmap = vec![0u8; 16 + 16 * self.blocks.len()];
        let pmap_len = pmap.len() as u32;
        put_u32(&mut pmap, 0, SECTION_PMAP);
        put_u32(&mut pmap, 4, pmap_len);
        put_u32(&mut pmap, 8, root_id);
        put_u16(&mut pmap, 12, self.blocks.len() as u16);
        put_u16(&mut pmap, 14, 0x7070);
        for (i, b) in self.blocks.iter().enumerate() {
            let r = 16 + i * 16;
            put_u32(&mut pmap, r, b.name);
            put_u32(&mut pmap, r + 4, offsets[i] as u32);
            put_u32(&mut pmap, r + 12, b.data.len() as u32);
        }

        let structs: Vec<PsoStructDef> = self.used_structs.iter().filter_map(|h| self.schema.pso_structs.get(h).cloned()).collect();
        let enums: Vec<crate::schema::PsoEnumDef> = self.used_enums.iter().filter_map(|h| self.schema.pso_enums.get(h).cloned()).collect();
        let count = structs.len() + enums.len();
        let mut psch = vec![0u8; 12 + 8 * count];
        let mut body: Vec<u8> = Vec::new();
        let mut index = Vec::with_capacity(count);
        for s in &structs {
            index.push((s.name, body.len()));
            let mut rec = vec![0u8; 12 + 12 * s.entries.len()];
            put_u32(&mut rec, 0, ((s.unk as u32) << 16) | s.entries.len() as u32);
            put_u32(&mut rec, 4, s.size);
            for (k, e) in s.entries.iter().enumerate() {
                let r = 12 + k * 12;
                put_u32(&mut rec, r, e.name);
                rec[r + 4] = e.ty;
                rec[r + 5] = e.subtype;
                put_u16(&mut rec, r + 6, e.offset);
                put_u32(&mut rec, r + 8, e.ref_key);
            }
            body.extend_from_slice(&rec);
        }
        for en in &enums {
            index.push((en.name, body.len()));
            let mut rec = vec![0u8; 4 + 8 * en.entries.len()];
            put_u32(&mut rec, 0, (1 << 24) | en.entries.len() as u32);
            for (k, (n, v)) in en.entries.iter().enumerate() {
                put_u32(&mut rec, 4 + k * 8, *n);
                put_u32(&mut rec, 8 + k * 8, *v as u32);
            }
            body.extend_from_slice(&rec);
        }
        let base = psch.len();
        put_u32(&mut psch, 0, SECTION_PSCH);
        put_u32(&mut psch, 4, (base + body.len()) as u32);
        put_u32(&mut psch, 8, count as u32);
        for (i, (name, off)) in index.iter().enumerate() {
            put_u32(&mut psch, 12 + i * 8, *name);
            put_u32(&mut psch, 16 + i * 8, (base + off) as u32);
        }
        psch.extend_from_slice(&body);

        let mut out = psin;
        out.extend_from_slice(&pmap);
        out.extend_from_slice(&psch);
        out
    }
}

fn type_name(ty: u8) -> &'static str {
    match ty {
        T_BOOL => "boolean",
        T_I8 | T_U8 | T_I16 | T_U16 | T_I32 | T_U32 | T_LONG | T_HFLOAT => "integer",
        T_F32 => "float",
        T_VEC2 => "vector2",
        T_VEC3 | T_VEC3A => "vector3",
        T_VEC4 | T_VEC4A => "vector4",
        T_STRING => "string or name",
        T_ENUM => "enum member",
        T_FLAGS => "flag set",
        T_STRUCT => "structure",
        T_ARRAY => "array",
        T_MAP => "map",
        _ => "value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_write::array;
    use crate::pso::{dump_pso, parse_pso};
    use crate::rage_joaat as j;
    use crate::xml::{from_xml, to_xml};

    fn st(ty: &str, fields: Vec<(&str, MetaValue)>) -> MetaValue {
        MetaValue::Struct(MetaStruct { type_hash: j(ty), fields: fields.into_iter().map(|(n, v)| (j(n), v)).collect() })
    }

    fn sample_manifest() -> MetaValue {
        st("CPackFileMetaData", vec![
            ("MapDataGroups", array(vec![st("CMapDataGroup", vec![
                ("Name", MetaValue::Str("casas_praia_extras".into())),
                ("Bounds", array(vec![MetaValue::Str("casas_praia_extras".into())])),
                ("Flags", MetaValue::U32(0)),
            ])])),
            ("HDTxdBindingArray", array(vec![])),
            ("imapDependencies", array(vec![])),
            ("imapDependencies_2", array(vec![st("CImapDependencies", vec![
                ("imapName", MetaValue::Str("casas_praia_extras".into())),
                ("manifestFlags", MetaValue::Str("INTERIOR_DATA".into())),
                ("itypDepArray", array(vec![MetaValue::Str("casas_praia_types".into()), MetaValue::Str("v_int_1".into())])),
            ])])),
            ("itypDependencies_2", array(vec![])),
            ("Interiors", array(vec![st("CInteriorBoundsFiles", vec![
                ("Name", MetaValue::Str("casas_praia_extras".into())),
                ("Bounds", array(vec![MetaValue::Str("casas_praia_extras".into())])),
            ])])),
        ])
    }

    #[test]
    fn a_manifest_round_trips_through_the_reader() {
        let w = build_pso(&sample_manifest(), Schema::builtin()).unwrap();
        assert!(w.warnings.is_empty(), "{:?}", w.warnings);
        let file = parse_pso(&w.bytes).unwrap();
        assert_eq!(file.root_type(), Some(j("CPackFileMetaData")));
        let dump = dump_pso(&w.bytes).unwrap();
        assert!(dump.warnings.is_empty(), "{:?}", dump.warnings);
        let root = dump.root.as_struct().unwrap();
        let deps = root.field("imapDependencies_2").unwrap().items();
        assert_eq!(deps.len(), 1);
        let dep = deps[0].as_struct().unwrap();
        assert_eq!(dep.field("imapName"), Some(&MetaValue::Hash(j("casas_praia_extras"))));
        assert_eq!(dep.field("itypDepArray").unwrap().items().len(), 2);
        assert!(matches!(dep.field("manifestFlags"), Some(MetaValue::Flags { bits, .. }) if *bits != 0), "{:?}", dep.field("manifestFlags"));

        // The manifest reader agrees.
        let (_, manifest) = crate::parse_ymf(&w.bytes).unwrap();
        assert_eq!(manifest.imap_dependencies_2.len(), 1);
        assert_eq!(manifest.imap_dependencies_2[0].name.hash, j("casas_praia_extras"));
    }

    #[test]
    fn dumped_xml_builds_back_to_the_same_dump() {
        let w = build_pso(&sample_manifest(), Schema::builtin()).unwrap();
        let dump1 = dump_pso(&w.bytes).unwrap();
        for names in [crate::names::NameTable::default(), crate::names::NameTable::core()] {
            let xml = to_xml(&dump1.root, &names);
            let value = from_xml(&xml).unwrap();
            let again = build_pso(&value, Schema::builtin()).unwrap();
            assert!(again.warnings.is_empty(), "{:?}", again.warnings);
            let dump2 = dump_pso(&again.bytes).unwrap();
            assert_eq!(to_xml(&dump2.root, &names), xml);
        }
    }

    #[test]
    fn the_files_own_schema_is_enough_to_rebuild_it() {
        let w = build_pso(&sample_manifest(), Schema::builtin()).unwrap();
        let own = Schema::from_file(&w.bytes).unwrap();
        assert!(own.pso_structs.contains_key(&j("CPackFileMetaData")));
        assert_eq!(own.pso_structs[&j("CImapDependencies")], Schema::builtin().pso_structs[&j("CImapDependencies")]);
        let dump = dump_pso(&w.bytes).unwrap();
        let again = build_pso(&dump.root, &own).unwrap();
        assert_eq!(dump_pso(&again.bytes).unwrap().root, dump.root);
    }
}
