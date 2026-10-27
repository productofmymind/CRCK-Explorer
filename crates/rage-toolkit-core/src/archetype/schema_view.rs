//! What the game's structure tables say about a value, for an editor: which members a structure has (in order), what each holds, which
//! names an enum or a flag set has, and how to turn typed text into a value of the right kind. Built on the same table as the
//! metadata validator (`crate::metaxml`).
use crate::metaxml::{meta_kind, pso_kind, Kind};
use anyhow::{bail, Result};
use rage_formats::schema::Schema;
use rage_formats::value::{MetaArray, MetaStruct};
use rage_formats::{rage_joaat, MetaValue, NameTable};
use rage_formats::math::{Vec2, Vec3, Vec4};

pub use crate::metaxml::Kind as FieldKind;

fn names() -> &'static NameTable {
    static N: std::sync::OnceLock<NameTable> = std::sync::OnceLock::new();
    N.get_or_init(NameTable::core)
}

#[derive(Debug, Clone)]
pub struct FieldDesc {
    pub name: String,
    pub hash: u32,
    pub kind: FieldKind,
}

/// The name of a structure, enum or member hash (`hash_XXXXXXXX` when unknown).
pub fn name_of(hash: u32) -> String {
    names().resolve(hash).into_owned()
}

/// The members of the structure called `type_hash`, in the order of the game's table. `None` for a structure the table lacks.
pub fn describe(type_hash: u32) -> Option<Vec<FieldDesc>> {
    let s = Schema::builtin();
    if let Some(m) = s.meta_structs.get(&type_hash) {
        return Some(m.entries.iter().filter(|e| e.name != 0x100).map(|e| FieldDesc { name: name_of(e.name), hash: e.name, kind: meta_kind(e, m) }).collect());
    }
    s.pso_structs.get(&type_hash).map(|p| p.entries.iter().filter(|e| e.name != 0x100).map(|e| FieldDesc { name: name_of(e.name), hash: e.name, kind: pso_kind(e, p) }).collect())
}

/// The structures of the game's tables whose name starts with `prefix` (`CExtensionDef`), by name: what a list of mixed items can hold.
pub fn struct_names_with_prefix(prefix: &str) -> Vec<(String, u32)> {
    let s = Schema::builtin();
    let mut v: Vec<(String, u32)> = s.meta_structs.keys().chain(s.pso_structs.keys()).map(|h| (name_of(*h), *h)).filter(|(n, _)| n.starts_with(prefix)).collect();
    v.sort();
    v.dedup();
    v
}

/// A new structure of type `type_hash` with every member the table lists, empty.
pub fn new_struct(type_hash: u32) -> MetaValue {
    let fields = describe(type_hash).unwrap_or_default().iter().map(|f| (f.hash, default_value(&f.kind))).collect();
    MetaValue::Struct(MetaStruct { type_hash, fields })
}

/// `(name hash, value)` of every member of an enum or flag set
pub fn enum_members(enum_hash: u32) -> Vec<(u32, i64)> {
    let s = Schema::builtin();
    if let Some(m) = s.meta_enums.get(&enum_hash) {
        return m.entries.iter().map(|(h, v)| (*h, *v as i64)).collect();
    }
    s.pso_enums.get(&enum_hash).map(|p| p.entries.iter().map(|(h, v)| (*h, *v as i64)).collect()).unwrap_or_default()
}

/// A value of the right kind with nothing in it (zero, empty, false).
pub fn default_value(kind: &FieldKind) -> MetaValue {
    match kind {
        Kind::Bool => MetaValue::Bool(false),
        Kind::Int(..) => MetaValue::I32(0),
        Kind::Float => MetaValue::F32(0.0),
        Kind::Vector(2) => MetaValue::Vec2(Vec2::new(0.0, 0.0)),
        Kind::Vector(3) => MetaValue::Vec3(Vec3::new(0.0, 0.0, 0.0)),
        Kind::Vector(_) => MetaValue::Vec4(Vec4::new(0.0, 0.0, 0.0, 0.0)),
        Kind::Text => MetaValue::Str(String::new()),
        Kind::Hash => MetaValue::Hash(0),
        Kind::Enum(h) => MetaValue::Enum { enum_hash: *h, value: 0, name: enum_members(*h).iter().find(|m| m.1 == 0).map(|m| m.0) },
        Kind::Flags(h) => MetaValue::Flags { enum_hash: *h, bits: 0, names: vec![] },
        Kind::Struct(h, _) => MetaValue::Struct(MetaStruct { type_hash: *h, fields: vec![] }),
        Kind::Array(_) => MetaValue::Array(MetaArray::default()),
        Kind::Other => MetaValue::Null,
    }
}

fn parse_int(s: &str) -> Option<i128> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i128::from_str_radix(h, 16).ok();
    }
    s.parse().ok()
}

/// The hash a typed name stands for: `hash_XXXXXXXX` and `0x...` are the number, anything else is hashed in lowercase (the game
/// looks names up in lowercase).
pub fn name_hash(text: &str) -> u32 {
    let t = text.trim();
    if t.is_empty() {
        return 0;
    }
    if let Some(h) = t.strip_prefix("hash_").or_else(|| t.strip_prefix("0x")) {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            return v;
        }
    }
    rage_joaat(&t.to_lowercase())
}

/// Turn what the user typed into a value of `kind`. `old` is the value being replaced (its number type is kept).
pub fn parse_text(kind: &FieldKind, text: &str, old: Option<&MetaValue>) -> Result<MetaValue> {
    let t = text.trim();
    Ok(match kind {
        Kind::Bool => match t.to_lowercase().as_str() {
            "true" | "1" | "yes" => MetaValue::Bool(true),
            "false" | "0" | "no" => MetaValue::Bool(false),
            _ => bail!("true or false"),
        },
        Kind::Int(lo, hi) => {
            let Some(n) = parse_int(t) else { bail!("a whole number (digits, or 0x... for hex)") };
            if n < *lo || n > *hi {
                bail!("a number from {lo} to {hi}");
            }
            // keep the kind of number that was there
            match old {
                Some(MetaValue::U8(_)) => MetaValue::U8(n as u8),
                Some(MetaValue::I8(_)) => MetaValue::I8(n as i8),
                Some(MetaValue::U16(_)) => MetaValue::U16(n as u16),
                Some(MetaValue::I16(_)) => MetaValue::I16(n as i16),
                Some(MetaValue::U32(_)) => MetaValue::U32(n as u32),
                Some(MetaValue::I64(_)) => MetaValue::I64(n as i64),
                Some(MetaValue::U64(_)) => MetaValue::U64(n as u64),
                _ => MetaValue::I32(n as i32),
            }
        }
        Kind::Float => MetaValue::F32(t.parse::<f32>().map_err(|_| anyhow::anyhow!("a number such as 1.5 (a point, not a comma)"))?),
        Kind::Vector(n) => {
            let parts: Vec<f32> = t.split(|c: char| c.is_whitespace() || c == ',').filter(|p| !p.is_empty()).map(|p| p.parse::<f32>()).collect::<std::result::Result<_, _>>().map_err(|_| anyhow::anyhow!("{n} numbers, e.g. 0 0 1"))?;
            if parts.len() != *n {
                bail!("{n} numbers, e.g. 0 0 1");
            }
            match n {
                2 => MetaValue::Vec2(Vec2::new(parts[0], parts[1])),
                3 => MetaValue::Vec3(Vec3::new(parts[0], parts[1], parts[2])),
                _ => MetaValue::Vec4(Vec4::new(parts[0], parts[1], parts[2], parts[3])),
            }
        }
        Kind::Text => MetaValue::Str(t.to_string()),
        Kind::Hash => MetaValue::Hash(name_hash(t)),
        Kind::Enum(h) => {
            let members = enum_members(*h);
            if let Some(m) = members.iter().find(|m| name_of(m.0).eq_ignore_ascii_case(t)) {
                MetaValue::Enum { enum_hash: *h, value: m.1 as i32, name: Some(m.0) }
            } else if let Some(n) = parse_int(t) {
                MetaValue::Enum { enum_hash: *h, value: n as i32, name: members.iter().find(|m| m.1 == n as i64).map(|m| m.0) }
            } else {
                bail!("one of: {}", members.iter().take(8).map(|m| name_of(m.0)).collect::<Vec<_>>().join(", "));
            }
        }
        Kind::Flags(h) => {
            let members = enum_members(*h);
            let mut bits = 0u32;
            for tok in t.split(|c: char| c.is_whitespace() || c == '|' || c == ',').filter(|x| !x.is_empty()) {
                if let Some(m) = members.iter().find(|m| name_of(m.0).eq_ignore_ascii_case(tok)) {
                    bits |= m.1 as u32;
                } else if let Some(n) = parse_int(tok) {
                    bits |= n as u32;
                } else {
                    bail!("flag names ({}) or a number", members.iter().take(5).map(|m| name_of(m.0)).collect::<Vec<_>>().join(", "));
                }
            }
            MetaValue::Flags { enum_hash: *h, bits, names: members.iter().filter(|m| m.1 as u32 & bits != 0 && m.1 != 0).map(|m| m.0).collect() }
        }
        Kind::Struct(..) | Kind::Array(_) | Kind::Other => bail!("this value is not edited as text"),
    })
}

/// Make a tree read from XML (loosely typed: names are text, enum members are words, untyped `<Item>`s) hold what the game's table says:
/// hashed names become hashes (in lowercase, as the game looks them up), enum and flag words become values, and a list item gets the
/// structure its list holds. The tree of a binary file already has all of this.
pub fn coerce_tree(v: &mut MetaValue, type_hash: u32) {
    let Some(descs) = describe(type_hash) else { return };
    let MetaValue::Struct(s) = v else { return };
    for (h, val) in s.fields.iter_mut() {
        let Some(d) = descs.iter().find(|d| d.hash == *h) else { continue };
        coerce_value(val, &d.kind);
    }
}

fn coerce_value(val: &mut MetaValue, kind: &FieldKind) {
    match (kind, &mut *val) {
        (Kind::Hash, MetaValue::Str(t)) => *val = MetaValue::Hash(name_hash(t)),
        (Kind::Enum(_) | Kind::Flags(_), MetaValue::Str(t)) => {
            if let Ok(parsed) = parse_text(kind, &t.clone(), None) {
                *val = parsed;
            }
        }
        (Kind::Struct(h, _), MetaValue::Struct(s)) => {
            // an untyped <Item> is the structure the list holds; a typed one keeps its own
            if (s.type_hash == rage_joaat("Item") || s.type_hash == 0) && *h != 0 {
                s.type_hash = *h;
            }
            let th = s.type_hash;
            coerce_tree(val, th);
        }
        (Kind::Array(elem), MetaValue::Array(a)) => {
            for item in a.items.iter_mut() {
                coerce_value(item, elem);
            }
        }
        _ => {}
    }
}

/// What a value looks like in a text box.
pub fn to_text(v: &MetaValue) -> String {
    use MetaValue as V;
    match v {
        V::Null => String::new(),
        V::Bool(b) => b.to_string(),
        V::I8(x) => x.to_string(),
        V::U8(x) => x.to_string(),
        V::I16(x) => x.to_string(),
        V::U16(x) => x.to_string(),
        V::I32(x) => x.to_string(),
        V::U32(x) => x.to_string(),
        V::I64(x) => x.to_string(),
        V::U64(x) => x.to_string(),
        V::F32(x) => rage_formats::xml::float(*x),
        V::Vec2(a) => format!("{} {}", rage_formats::xml::float(a.x), rage_formats::xml::float(a.y)),
        V::Vec3(a) => format!("{} {} {}", rage_formats::xml::float(a.x), rage_formats::xml::float(a.y), rage_formats::xml::float(a.z)),
        V::Vec4(a) => format!("{} {} {} {}", rage_formats::xml::float(a.x), rage_formats::xml::float(a.y), rage_formats::xml::float(a.z), rage_formats::xml::float(a.w)),
        V::Hash(h) => {
            if *h == 0 {
                String::new()
            } else {
                name_of(*h)
            }
        }
        V::Str(s) => s.clone(),
        V::Enum { value, name, .. } => name.map(name_of).unwrap_or_else(|| value.to_string()),
        V::Flags { bits, names, .. } => {
            if names.is_empty() {
                format!("{bits}")
            } else {
                names.iter().map(|n| name_of(*n)).collect::<Vec<_>>().join(" | ")
            }
        }
        V::Bytes(b) => format!("{} bytes", b.len()),
        V::Array(a) => format!("{} item(s)", a.items.len()),
        V::Map(m) => format!("{} entries", m.len()),
        V::Struct(s) => name_of(s.type_hash),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_members_of_an_archetype_come_in_the_games_order_with_their_kinds() {
        let f = describe(rage_joaat("CBaseArchetypeDef")).expect("CBaseArchetypeDef is in the table");
        let names: Vec<&str> = f.iter().map(|x| x.name.as_str()).collect();
        assert!(names.starts_with(&["lodDist", "flags", "specialAttribute"]), "{names:?}");
        let kind = |n: &str| f.iter().find(|x| x.name == n).unwrap().kind.clone();
        assert!(matches!(kind("lodDist"), Kind::Float));
        assert!(matches!(kind("bbMin"), Kind::Vector(3)));
        assert!(matches!(kind("assetType"), Kind::Enum(_)));
        assert!(matches!(kind("name"), Kind::Hash));
        assert!(matches!(kind("extensions"), Kind::Array(_)));
        assert!(describe(rage_joaat("CMloRoomDef")).is_some() && describe(rage_joaat("CMloPortalDef")).is_some());
    }

    #[test]
    fn typed_text_becomes_a_value_of_the_right_kind_or_a_clear_error() {
        let f = |k: &FieldKind, t: &str| parse_text(k, t, None);
        assert_eq!(f(&Kind::Float, " 12.5 ").unwrap(), MetaValue::F32(12.5));
        assert!(f(&Kind::Float, "1,5").is_err());
        assert_eq!(f(&Kind::Int(0, 255), "0x10").unwrap(), MetaValue::I32(16));
        assert!(f(&Kind::Int(0, 255), "300").unwrap_err().to_string().contains("0 to 255"));
        assert_eq!(parse_text(&Kind::Int(0, 255), "7", Some(&MetaValue::U8(1))).unwrap(), MetaValue::U8(7), "keeps the number type");
        assert_eq!(f(&Kind::Vector(3), "1 2, 3").unwrap(), MetaValue::Vec3(Vec3::new(1.0, 2.0, 3.0)));
        assert!(f(&Kind::Vector(3), "1 2").is_err());
        assert_eq!(f(&Kind::Hash, "Prop_Box_01").unwrap(), MetaValue::Hash(rage_joaat("prop_box_01")), "names are hashed in lowercase");
        assert_eq!(f(&Kind::Text, "Prop_Box_01").unwrap(), MetaValue::Str("Prop_Box_01".into()), "a plain string stays text");
        assert_eq!(f(&Kind::Hash, "hash_DEADBEEF").unwrap(), MetaValue::Hash(0xDEADBEEF));
        let enum_hash = describe(rage_joaat("CBaseArchetypeDef")).unwrap().into_iter().find(|x| x.name == "assetType").map(|x| if let Kind::Enum(h) = x.kind { h } else { 0 }).unwrap();
        let v = f(&Kind::Enum(enum_hash), "asset_type_drawable").unwrap();
        assert_eq!(to_text(&v), "ASSET_TYPE_DRAWABLE");
        assert!(f(&Kind::Enum(enum_hash), "nonsense").unwrap_err().to_string().contains("ASSET_TYPE"));
        assert_eq!(f(&Kind::Bool, "yes").unwrap(), MetaValue::Bool(true));
    }

    #[test]
    fn a_tree_read_from_xml_is_made_to_hold_what_the_table_says() {
        let mut t = rage_formats::from_xml(r#"<CMapTypes><archetypes><Item type="CMloArchetypeDef"><name>My_Interior</name><assetType>asset_type_assetless</assetType><rooms><Item><name>limbo</name></Item></rooms></Item></archetypes></CMapTypes>"#).unwrap();
        coerce_tree(&mut t, rage_joaat("CMapTypes"));
        let a = t.as_struct().unwrap().field("archetypes").unwrap().items()[0].as_struct().unwrap().clone();
        assert_eq!(a.field("name"), Some(&MetaValue::Hash(rage_joaat("my_interior"))), "hashed in lowercase");
        assert!(matches!(a.field("assetType"), Some(MetaValue::Enum { .. })), "{:?}", a.field("assetType"));
        let room = a.field("rooms").unwrap().items()[0].as_struct().unwrap();
        assert_eq!(room.type_hash, rage_joaat("CMloRoomDef"), "an untyped item is what its list holds");
    }
}
