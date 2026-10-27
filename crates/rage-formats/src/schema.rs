//! Structure and enum definitions for writing Meta and PSO files.
//!
//! A Meta or PSO file carries the schema of the structures it holds, so a
//! reader never needs one. A writer building a file from XML or JSON does:
//! [`Schema::builtin`] is the reference tool's table of the game's structures (the
//! names are the game's own identifiers), extracted by
//! `tools/extract_reference_schema.py`, and [`Schema::from_file`] takes
//! the definitions an existing binary file carries, which win over the
//! built-in ones when merged.
use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use crate::pso::{u16_be, u32_be, PSO_MAGIC};
use crate::resource::{prepare_rsc7, u16_le, u32_le, u64_le, ResReader, RSC7_MAGIC, SYSTEM_BASE};

/// The pseudo-name of an array's element descriptor entry.
pub const ARRAYINFO: u32 = 0x100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaEntryDef {
    pub name: u32,
    pub offset: u32,
    pub ty: u8,
    pub unk9: u8,
    /// For an array member, the index of its `ARRAYINFO` entry.
    pub ref_index: i16,
    /// A structure or enum name hash, or a length, depending on the type.
    pub ref_key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaStructDef {
    pub name: u32,
    pub key: u32,
    pub unk8: u32,
    pub size: u32,
    pub entries: Vec<MetaEntryDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaEnumDef {
    pub name: u32,
    pub key: u32,
    pub entries: Vec<(u32, i32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoEntryDef {
    pub name: u32,
    pub ty: u8,
    pub subtype: u8,
    pub offset: u16,
    pub ref_key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoStructDef {
    pub name: u32,
    pub ty: u8,
    pub unk: u8,
    pub size: u32,
    pub entries: Vec<PsoEntryDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsoEnumDef {
    pub name: u32,
    pub ty: u8,
    pub entries: Vec<(u32, i32)>,
}

impl MetaStructDef {
    pub fn entry(&self, name: u32) -> Option<&MetaEntryDef> {
        self.entries.iter().find(|e| e.name == name)
    }
}

impl PsoStructDef {
    pub fn entry(&self, name: u32) -> Option<&PsoEntryDef> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// Everything a writer needs to know about the structures it may meet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    pub meta_structs: HashMap<u32, MetaStructDef>,
    pub meta_enums: HashMap<u32, MetaEnumDef>,
    pub pso_structs: HashMap<u32, PsoStructDef>,
    pub pso_enums: HashMap<u32, PsoEnumDef>,
}

const META_TABLE: &str = include_str!("schema/reference_meta.txt");
const PSO_TABLE: &str = include_str!("schema/reference_pso.txt");

impl Schema {
    /// The reference tool's tables, parsed once.
    pub fn builtin() -> &'static Schema {
        static BUILTIN: OnceLock<Schema> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            let mut s = Schema::parse_table(META_TABLE).expect("built-in meta table parses");
            s.merge(Schema::parse_table(PSO_TABLE).expect("built-in pso table parses"));
            s
        })
    }

    /// Parses the text table format `tools/extract_reference_schema.py`
    /// writes (see that script's docstring).
    pub fn parse_table(text: &str) -> Result<Schema> {
        let mut schema = Schema::default();
        let hex = |s: &str| u32::from_str_radix(s, 16).with_context(|| format!("bad hash '{s}'"));
        let num = |s: &str| s.parse::<i64>().with_context(|| format!("bad number '{s}'"));
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let ctx = || format!("schema table line {}", lineno + 1);
            let mut tok = line.split_whitespace();
            let kind = tok.next().unwrap();
            match kind {
                "M" => {
                    let name = hex(tok.next().context("struct name").with_context(ctx)?)?;
                    let key = hex(tok.next().context("key").with_context(ctx)?)?;
                    let unk8 = num(tok.next().context("unk8").with_context(ctx)?)? as u32;
                    let size = num(tok.next().context("size").with_context(ctx)?)? as u32;
                    let mut entries = Vec::new();
                    for e in tok {
                        let f: Vec<&str> = e.split(',').collect();
                        if f.len() != 6 {
                            bail!("{}: malformed member '{e}'", ctx());
                        }
                        entries.push(MetaEntryDef {
                            name: hex(f[0])?,
                            offset: num(f[1])? as u32,
                            ty: hex(f[2])? as u8,
                            unk9: num(f[3])? as u8,
                            ref_index: num(f[4])? as i16,
                            ref_key: hex(f[5])?,
                        });
                    }
                    schema.meta_structs.insert(name, MetaStructDef { name, key, unk8, size, entries });
                }
                "ME" => {
                    let name = hex(tok.next().context("enum name").with_context(ctx)?)?;
                    let key = hex(tok.next().context("key").with_context(ctx)?)?;
                    let entries = tok.map(|e| -> Result<(u32, i32)> {
                        let (n, v) = e.split_once(',').with_context(|| format!("{}: malformed enum member '{e}'", ctx()))?;
                        Ok((hex(n)?, num(v)? as i32))
                    }).collect::<Result<Vec<_>>>()?;
                    schema.meta_enums.insert(name, MetaEnumDef { name, key, entries });
                }
                "P" => {
                    let name = hex(tok.next().context("struct name").with_context(ctx)?)?;
                    let ty = num(tok.next().context("type").with_context(ctx)?)? as u8;
                    let unk = num(tok.next().context("unk").with_context(ctx)?)? as u8;
                    let size = num(tok.next().context("size").with_context(ctx)?)? as u32;
                    let mut entries = Vec::new();
                    for e in tok {
                        let f: Vec<&str> = e.split(',').collect();
                        if f.len() != 5 {
                            bail!("{}: malformed member '{e}'", ctx());
                        }
                        entries.push(PsoEntryDef {
                            name: hex(f[0])?,
                            ty: hex(f[1])? as u8,
                            offset: num(f[2])? as u16,
                            subtype: num(f[3])? as u8,
                            ref_key: hex(f[4])?,
                        });
                    }
                    schema.pso_structs.insert(name, PsoStructDef { name, ty, unk, size, entries });
                }
                "PE" => {
                    let name = hex(tok.next().context("enum name").with_context(ctx)?)?;
                    let ty = num(tok.next().context("type").with_context(ctx)?)? as u8;
                    let entries = tok.map(|e| -> Result<(u32, i32)> {
                        let (n, v) = e.split_once(',').with_context(|| format!("{}: malformed enum member '{e}'", ctx()))?;
                        Ok((hex(n)?, num(v)? as i32))
                    }).collect::<Result<Vec<_>>>()?;
                    schema.pso_enums.insert(name, PsoEnumDef { name, ty, entries });
                }
                other => bail!("{}: unknown record kind '{other}'", ctx()),
            }
        }
        Ok(schema)
    }

    /// The definitions an existing RSC7 Meta or PSO file carries, every
    /// field included (the readers' tables drop the ones only a writer needs).
    pub fn from_file(data: &[u8]) -> Result<Schema> {
        if data.len() >= 4 && u32_le(data, 0) == RSC7_MAGIC {
            let (system, graphics) = prepare_rsc7(data)?;
            let reader = ResReader { system: &system, graphics: &graphics };
            return Self::from_meta_reader(&reader);
        }
        if data.len() >= 4 && u32_be(data, 0) == PSO_MAGIC {
            return Self::from_pso(data);
        }
        bail!("not an RSC7 Meta or PSO file; only those carry a schema")
    }

    fn from_meta_reader(reader: &ResReader<'_>) -> Result<Schema> {
        let header = reader.resolve(SYSTEM_BASE, 0x70).context("meta: header out of bounds")?;
        if u32_le(header, 0x10) != 0x5052_4430 {
            bail!("RSC7 file is not a Meta file (no 'PRD0' marker)");
        }
        let struct_ptr = u64_le(header, 0x20);
        let enum_ptr = u64_le(header, 0x28);
        let struct_count = u16_le(header, 0x48) as usize;
        let enum_count = u16_le(header, 0x4A) as usize;
        let mut schema = Schema::default();
        if struct_count > 0 {
            let table = reader.resolve(struct_ptr, struct_count * 32).context("meta: structure info table out of bounds")?;
            for rec in table.chunks_exact(32) {
                let name = u32_le(rec, 0);
                let entries_ptr = u64_le(rec, 0x10);
                let count = u16_le(rec, 0x1E) as usize;
                let bytes = if count > 0 { reader.resolve(entries_ptr, count * 16).context("meta: members out of bounds")? } else { &[][..] };
                let entries = bytes.chunks_exact(16).map(|e| MetaEntryDef {
                    name: u32_le(e, 0),
                    offset: u32_le(e, 4),
                    ty: e[8],
                    unk9: e[9],
                    ref_index: u16_le(e, 10) as i16,
                    ref_key: u32_le(e, 12),
                }).collect();
                schema.meta_structs.insert(name, MetaStructDef { name, key: u32_le(rec, 4), unk8: u32_le(rec, 8), size: u32_le(rec, 0x18), entries });
            }
        }
        if enum_count > 0 {
            let table = reader.resolve(enum_ptr, enum_count * 24).context("meta: enum info table out of bounds")?;
            for rec in table.chunks_exact(24) {
                let name = u32_le(rec, 0);
                let entries_ptr = u64_le(rec, 8);
                let count = u32_le(rec, 0x10) as usize;
                let bytes = if count > 0 { reader.resolve(entries_ptr, count * 8).context("meta: enum members out of bounds")? } else { &[][..] };
                let entries = bytes.chunks_exact(8).map(|e| (u32_le(e, 0), u32_le(e, 4) as i32)).collect();
                schema.meta_enums.insert(name, MetaEnumDef { name, key: u32_le(rec, 4), entries });
            }
        }
        Ok(schema)
    }

    fn from_pso(data: &[u8]) -> Result<Schema> {
        const SECTION_PSCH: u32 = 0x5053_4348;
        let mut schema = Schema::default();
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            let ident = u32_be(data, pos);
            let length = u32_be(data, pos + 4) as usize;
            if length < 8 || pos + length > data.len() {
                bail!("pso: section {ident:#010x} at {pos} runs past the end of the file");
            }
            if ident == SECTION_PSCH {
                let section = &data[pos..pos + length];
                let count = u32_be(section, 8) as usize;
                for i in 0..count {
                    let name = u32_be(section, 12 + i * 8);
                    let offset = u32_be(section, 16 + i * 8) as usize;
                    let word = u32_be(section, offset);
                    match word >> 24 {
                        0 => {
                            let n = (word & 0xFFFF) as usize;
                            let entries = (0..n).map(|k| {
                                let e = offset + 12 + k * 12;
                                PsoEntryDef { name: u32_be(section, e), ty: section[e + 4], subtype: section[e + 5], offset: u16_be(section, e + 6), ref_key: u32_be(section, e + 8) }
                            }).collect();
                            schema.pso_structs.insert(name, PsoStructDef { name, ty: 0, unk: ((word >> 16) & 0xFF) as u8, size: u32_be(section, offset + 4), entries });
                        }
                        1 => {
                            let n = (word & 0x00FF_FFFF) as usize;
                            let entries = (0..n).map(|k| (u32_be(section, offset + 4 + k * 8), u32_be(section, offset + 8 + k * 8) as i32)).collect();
                            schema.pso_enums.insert(name, PsoEnumDef { name, ty: 1, entries });
                        }
                        other => bail!("pso: unknown schema entry type {other}"),
                    }
                }
            }
            pos += length;
        }
        Ok(schema)
    }

    /// Adds `other`'s definitions, replacing any of the same name.
    pub fn merge(&mut self, other: Schema) {
        self.meta_structs.extend(other.meta_structs);
        self.meta_enums.extend(other.meta_enums);
        self.pso_structs.extend(other.pso_structs);
        self.pso_enums.extend(other.pso_enums);
    }

    pub fn is_empty(&self) -> bool {
        self.meta_structs.is_empty() && self.pso_structs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rage_joaat;

    #[test]
    fn builtin_tables_load_and_know_the_map_structures() {
        let s = Schema::builtin();
        let map = s.meta_structs.get(&rage_joaat("CMapData")).expect("CMapData");
        assert_eq!(map.size, 512);
        assert_eq!(map.key, 3448101671);
        let entities = map.entry(rage_joaat("entities")).expect("entities");
        assert_eq!((entities.offset, entities.ty, entities.ref_index), (96, 0x52, 8));
        assert_eq!(map.entries[8].name, ARRAYINFO);
        assert_eq!(map.entries[8].ty, 0x07);
        assert!(s.meta_structs.contains_key(&rage_joaat("CEntityDef")));

        let manifest = s.pso_structs.get(&rage_joaat("CPackFileMetaData")).expect("CPackFileMetaData");
        assert!(manifest.entries.len() >= 3);
        let map = s.pso_structs.get(&rage_joaat("CMapData")).expect("pso CMapData");
        assert_eq!(map.size, 304);
        assert!(s.meta_structs.len() > 60 && s.pso_structs.len() > 900, "{} {}", s.meta_structs.len(), s.pso_structs.len());
    }

    #[test]
    fn merge_prefers_the_newcomer() {
        let mut a = Schema::parse_table("M 1 2 1024 16 3,0,15,0,0,0\n").unwrap();
        let b = Schema::parse_table("M 1 2 1024 32 3,0,15,0,0,0\nME 9 0 a,1 b,2\n").unwrap();
        a.merge(b);
        assert_eq!(a.meta_structs[&1].size, 32);
        assert_eq!(a.meta_enums[&9].entries, vec![(0xa, 1), (0xb, 2)]);
    }

    #[test]
    fn a_files_own_schema_round_trips_through_the_meta_writer() {
        // Built later in meta_write's tests; here only the reader side.
        assert!(Schema::from_file(b"nope").is_err());
    }
}
