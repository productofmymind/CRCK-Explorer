//! Reading typed structures out of the generic tree ([`MetaValue`]), whichever
//! container the file came in. The vehicle and ped metadata files exist in two
//! forms — PSO `.ymt` (`update.rpf/x64/data/carcols.ymt`) and XML `.meta`
//! (`dlc.rpf/common/data/carcols.meta`) — and the reference tool reads both by turning
//! the PSO into XML and parsing the XML (`CarColsFile.Load`: "yep let's just
//! convert that to XML"). Here both become the same tree, and these helpers
//! absorb what still differs: an XML scalar arrives as `I32`/`F32`/`Str`, a
//! PSO scalar as its exact type; XML `<indices>0 1 2</indices>` is a string,
//! PSO's is a byte array; an XML enum is its name, a PSO enum its value with
//! the member's name hash.

use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use crate::coerce;
use crate::hash::rage_joaat;
use crate::math::{Vec2, Vec3, Vec4};
use crate::names::NameTable;
use crate::value::{MetaStruct, MetaValue};

const RSC7_MAGIC: u32 = 0x37435352;

/// The built-in names, built once: a file has thousands of hash and enum
/// members and `NameTable::core()` is a table of tens of thousands of names.
fn names() -> &'static NameTable {
    static NAMES: OnceLock<NameTable> = OnceLock::new();
    NAMES.get_or_init(NameTable::core)
}

/// Parses a metadata file of any container: a PSO stream, an RSC7-wrapped
/// Meta resource, or UTF-8 XML in the layout the reference tool writes (which the
/// game's own `.meta` files share). The root value is returned.
pub fn parse_tree(data: &[u8]) -> Result<MetaValue> {
    if crate::pso::is_pso(data) {
        return Ok(crate::pso::dump_pso(data)?.root);
    }
    if data.len() >= 4 && u32::from_le_bytes(data[0..4].try_into().unwrap()) == RSC7_MAGIC {
        return Ok(crate::meta_schema::dump_meta(data)?.root);
    }
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let text = std::str::from_utf8(data).context("not PSO, RSC7 or UTF-8 XML")?;
    crate::xml::from_xml(text)
}

/// The root as a structure of type `expected`, or an error naming what was
/// found instead.
pub fn root_struct<'a>(root: &'a MetaValue, expected: &str) -> Result<&'a MetaStruct> {
    let Some(s) = root.as_struct() else { bail!("the root is not a structure (expected {expected})") };
    if s.type_hash != rage_joaat(expected) {
        bail!("the root is {}, expected {expected}", names().resolve(s.type_hash));
    }
    Ok(s)
}

/// An enumeration the reference tool names: a value with, usually, a name.
pub trait MetaEnum: Sized + Copy {
    fn from_value(value: i32) -> Self;
    fn from_name(name: &str) -> Option<Self>;
    fn value(self) -> i32;
    fn name(self) -> Option<&'static str>;
}

/// Defines a [`MetaEnum`] with the reference tool's member names and values plus an
/// `Unknown(i32)` for values the definition does not list.
#[macro_export]
macro_rules! meta_enum {
    ($(#[$m:meta])* $vis:vis enum $E:ident { $($(#[$vm:meta])* $V:ident = $val:expr),* $(,)? }) => {
        $(#[$m])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis enum $E {
            $($(#[$vm])* $V,)*
            Unknown(i32),
        }
        impl $crate::meta_read::MetaEnum for $E {
            fn from_value(value: i32) -> Self {
                match value {
                    $(x if x == $val => $E::$V,)*
                    other => $E::Unknown(other),
                }
            }
            fn from_name(name: &str) -> Option<Self> {
                match name {
                    $(stringify!($V) => Some($E::$V),)*
                    _ => None,
                }
            }
            fn value(self) -> i32 {
                match self {
                    $($E::$V => $val,)*
                    $E::Unknown(v) => v,
                }
            }
            fn name(self) -> Option<&'static str> {
                match self {
                    $($E::$V => Some(stringify!($V)),)*
                    $E::Unknown(_) => None,
                }
            }
        }
        impl Default for $E {
            fn default() -> Self { <$E as $crate::meta_read::MetaEnum>::from_value(0) }
        }
    };
}

/// Typed access to a structure's members by name, tolerant of both forms.
pub trait Fields {
    fn value(&self, name: &str) -> Option<&MetaValue>;

    /// A nested structure.
    fn child(&self, name: &str) -> Option<&MetaStruct> {
        self.value(name).and_then(MetaValue::as_struct)
    }

    /// The elements of an array member; empty for anything else.
    fn items(&self, name: &str) -> &[MetaValue] {
        self.value(name).map_or(&[], MetaValue::items)
    }

    /// The nested structures of an array member.
    fn structs(&self, name: &str) -> Vec<&MetaStruct> {
        self.items(name).iter().filter_map(MetaValue::as_struct).collect()
    }

    /// A string member: the text, a hash written as `hash_XXXXXXXX` (or its
    /// name when the reference tool's list knows it), an enum's member name.
    fn text(&self, name: &str) -> String {
        self.value(name).map(text_of).unwrap_or_default()
    }

    /// A name member as its JOAAT hash (a `hash_XXXXXXXX` literal, a stored
    /// hash, or the hash of the text). 0 when absent or empty.
    fn hash(&self, name: &str) -> u32 {
        self.value(name).and_then(coerce::hash_of).unwrap_or(0)
    }

    fn i64(&self, name: &str) -> i64 {
        self.value(name).and_then(number_of).unwrap_or(0)
    }

    fn i32(&self, name: &str) -> i32 {
        self.i64(name) as i32
    }

    fn u32(&self, name: &str) -> u32 {
        self.i64(name) as u32
    }

    fn u16(&self, name: &str) -> u16 {
        self.i64(name) as u16
    }

    fn u8(&self, name: &str) -> u8 {
        self.i64(name) as u8
    }

    fn i8(&self, name: &str) -> i8 {
        self.i64(name) as i8
    }

    fn f32(&self, name: &str) -> f32 {
        self.value(name).and_then(float_of).unwrap_or(0.0)
    }

    fn bool(&self, name: &str) -> bool {
        self.value(name).and_then(bool_of).unwrap_or(false)
    }

    fn vec2(&self, name: &str) -> Vec2 {
        match self.value(name) {
            Some(MetaValue::Vec2(v)) => *v,
            Some(MetaValue::Vec3(v)) => Vec2::new(v.x, v.y),
            Some(MetaValue::Vec4(v)) => Vec2::new(v.x, v.y),
            _ => Vec2::new(0.0, 0.0),
        }
    }

    fn vec3(&self, name: &str) -> Vec3 {
        self.value(name).and_then(MetaValue::as_vec3).unwrap_or(Vec3::ZERO)
    }

    fn vec4(&self, name: &str) -> Vec4 {
        match self.value(name) {
            Some(MetaValue::Vec4(v)) => *v,
            Some(MetaValue::Vec3(v)) => Vec4::new(v.x, v.y, v.z, 0.0),
            _ => Vec4::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    /// An enum member, from its value (PSO) or its name (XML).
    fn enum_of<E: MetaEnum>(&self, name: &str) -> E {
        self.value(name).map(enum_value).unwrap_or_else(|| E::from_value(0))
    }

    /// The strings of an array of `<Item>` texts.
    fn strings(&self, name: &str) -> Vec<String> {
        self.items(name).iter().map(text_of).collect()
    }

    /// The hashes of an array of names.
    fn hashes(&self, name: &str) -> Vec<u32> {
        self.items(name).iter().filter_map(coerce::hash_of).collect()
    }

    /// The words of a space-separated text (`<flags>A B C</flags>`), or the
    /// texts of an array.
    fn words(&self, name: &str) -> Vec<String> {
        match self.value(name) {
            Some(MetaValue::Str(s)) => s.split_whitespace().map(str::to_owned).collect(),
            Some(v) => v.items().iter().map(text_of).collect(),
            None => Vec::new(),
        }
    }

    /// A list of integers: an array of numbers, a byte array, or a text of
    /// whitespace-separated numbers (`<indices content="char_array">`).
    fn numbers(&self, name: &str) -> Vec<i64> {
        match self.value(name) {
            Some(MetaValue::Bytes(b)) => b.iter().map(|b| i64::from(*b)).collect(),
            Some(MetaValue::Str(s)) => s.split_whitespace().filter_map(|w| w.parse().ok()).collect(),
            Some(v) => v.items().iter().filter_map(number_of).collect(),
            None => Vec::new(),
        }
    }

    fn bytes(&self, name: &str) -> Vec<u8> {
        self.numbers(name).into_iter().map(|n| n as u8).collect()
    }

    /// A list of booleans: `<Item value="true"/>` items, PSO booleans, or
    /// numbers where anything but 0 is true (the reference tool's `liveries` fallback).
    fn bools(&self, name: &str) -> Vec<bool> {
        match self.value(name) {
            Some(MetaValue::Str(s)) => s.split_whitespace().filter_map(|w| w.parse::<i64>().ok()).map(|n| n != 0).collect(),
            Some(v) => v.items().iter().filter_map(bool_of).collect(),
            None => Vec::new(),
        }
    }

    fn floats(&self, name: &str) -> Vec<f32> {
        match self.value(name) {
            Some(MetaValue::Str(s)) => s.split_whitespace().filter_map(|w| w.parse().ok()).collect(),
            Some(v) => v.items().iter().filter_map(float_of).collect(),
            None => Vec::new(),
        }
    }
}

impl Fields for MetaStruct {
    fn value(&self, name: &str) -> Option<&MetaValue> {
        self.field(name)
    }
}

/// A value as the reference tool's XML text would show it.
pub fn text_of(v: &MetaValue) -> String {
    match v {
        MetaValue::Str(s) => s.clone(),
        MetaValue::Hash(0) | MetaValue::Null => String::new(),
        MetaValue::Hash(h) => names().resolve(*h).into_owned(),
        MetaValue::Enum { name: Some(member), .. } => names().resolve(*member).into_owned(),
        MetaValue::Enum { value, .. } => value.to_string(),
        MetaValue::Bool(b) => b.to_string(),
        MetaValue::F32(f) => crate::xml::float(*f),
        other => number_of(other).map(|n| n.to_string()).unwrap_or_default(),
    }
}

fn number_of(v: &MetaValue) -> Option<i64> {
    match v {
        MetaValue::F32(f) => Some(*f as i64),
        MetaValue::Str(s) => {
            let s = s.trim();
            s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64)).or_else(|| {
                s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).and_then(|h| i64::from_str_radix(h, 16).ok())
            })
        }
        other => other.as_i64(),
    }
}

fn float_of(v: &MetaValue) -> Option<f32> {
    match v {
        MetaValue::Str(s) => s.trim().parse().ok(),
        other => other.as_f32(),
    }
}

fn bool_of(v: &MetaValue) -> Option<bool> {
    match v {
        MetaValue::Bool(b) => Some(*b),
        MetaValue::Str(s) => match s.trim() {
            "true" | "True" | "TRUE" => Some(true),
            "false" | "False" | "FALSE" => Some(false),
            other => other.parse::<i64>().ok().map(|n| n != 0),
        },
        other => other.as_i64().map(|n| n != 0),
    }
}

fn enum_value<E: MetaEnum>(v: &MetaValue) -> E {
    match v {
        MetaValue::Enum { value, .. } => E::from_value(*value),
        MetaValue::Str(s) => {
            let s = s.trim();
            E::from_name(s).unwrap_or_else(|| E::from_value(s.parse().unwrap_or(0)))
        }
        other => E::from_value(other.as_i64().unwrap_or(0) as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::MetaArray;

    meta_enum! {
        pub enum Sample { first = 0, second = 1, minus = -1 }
    }

    fn s(fields: Vec<(&str, MetaValue)>) -> MetaStruct {
        MetaStruct { type_hash: 1, fields: fields.into_iter().map(|(n, v)| (rage_joaat(n), v)).collect() }
    }

    fn array(items: Vec<MetaValue>) -> MetaValue {
        MetaValue::Array(MetaArray { item_type: None, typed_items: false, items })
    }

    #[test]
    fn scalars_read_from_either_form() {
        let xml = s(vec![("a", MetaValue::I32(7)), ("b", MetaValue::Str("3.5".into())), ("c", MetaValue::Str("true".into())), ("d", MetaValue::F32(2.0))]);
        assert_eq!(xml.u32("a"), 7);
        assert_eq!(xml.f32("b"), 3.5);
        assert!(xml.bool("c"));
        assert_eq!(xml.i64("d"), 2);
        let pso = s(vec![("a", MetaValue::U8(7)), ("b", MetaValue::F32(3.5)), ("c", MetaValue::Bool(true))]);
        assert_eq!(pso.u8("a"), 7);
        assert_eq!(pso.f32("b"), 3.5);
        assert!(pso.bool("c"));
        assert_eq!(pso.u32("missing"), 0);
    }

    #[test]
    fn lists_read_from_text_bytes_or_arrays() {
        let v = s(vec![
            ("indices", MetaValue::Str("5\t\n111 3".into())),
            ("bytes", MetaValue::Bytes(vec![1, 2])),
            ("items", array(vec![MetaValue::U8(4), MetaValue::U8(5)])),
            ("liveries", array(vec![MetaValue::Bool(true), MetaValue::Bool(false)])),
            ("liveries_text", MetaValue::Str("0 1".into())),
            ("flags", MetaValue::Str("FLAG_A FLAG_B".into())),
            ("names", array(vec![MetaValue::Str("x".into()), MetaValue::Hash(rage_joaat("y"))])),
            ("dists", MetaValue::Str("10.0\n25.0".into())),
        ]);
        assert_eq!(v.numbers("indices"), vec![5, 111, 3]);
        assert_eq!(v.bytes("bytes"), vec![1, 2]);
        assert_eq!(v.numbers("items"), vec![4, 5]);
        assert_eq!(v.bools("liveries"), vec![true, false]);
        assert_eq!(v.bools("liveries_text"), vec![false, true]);
        assert_eq!(v.words("flags"), vec!["FLAG_A", "FLAG_B"]);
        assert_eq!(v.hashes("names"), vec![rage_joaat("x"), rage_joaat("y")]);
        assert_eq!(v.floats("dists"), vec![10.0, 25.0]);
    }

    #[test]
    fn enums_read_by_name_or_value() {
        let v = s(vec![
            ("x", MetaValue::Str("second".into())),
            ("y", MetaValue::Enum { enum_hash: 0, value: -1, name: None }),
            ("z", MetaValue::Str("nope".into())),
            ("w", MetaValue::I32(9)),
        ]);
        assert_eq!(v.enum_of::<Sample>("x"), Sample::second);
        assert_eq!(v.enum_of::<Sample>("y"), Sample::minus);
        assert_eq!(v.enum_of::<Sample>("z"), Sample::first);
        assert_eq!(v.enum_of::<Sample>("w"), Sample::Unknown(9));
        assert_eq!(Sample::minus.name(), Some("minus"));
        assert_eq!(Sample::Unknown(9).value(), 9);
    }

    #[test]
    fn text_shows_names_for_hashes_and_enums() {
        let v = s(vec![
            ("name", MetaValue::Str("adder".into())),
            ("h", MetaValue::Hash(rage_joaat("CVehicleKit"))),
            ("u", MetaValue::Hash(0xDEAD_BEEF)),
            ("e", MetaValue::Enum { enum_hash: 0, value: 3, name: Some(rage_joaat("MKT_SPECIAL")) }),
        ]);
        assert_eq!(v.text("name"), "adder");
        assert_eq!(v.text("h"), "CVehicleKit");
        assert_eq!(v.text("u"), "hash_DEADBEEF");
        assert_eq!(v.text("e"), "MKT_SPECIAL");
        assert_eq!(v.hash("name"), rage_joaat("adder"));
    }

    #[test]
    fn parse_tree_reads_xml_and_checks_the_root() {
        let root = parse_tree(b"\xEF\xBB\xBF<CVehicleModColors><metallic/></CVehicleModColors>").unwrap();
        assert!(root_struct(&root, "CVehicleModColors").is_ok());
        let err = root_struct(&root, "CVehicleModelInfoVarGlobal").unwrap_err().to_string();
        assert!(err.contains("CVehicleModColors"), "{err}");
        assert!(parse_tree(&[0xFF, 0xFE, 1, 2]).is_err());
    }
}
