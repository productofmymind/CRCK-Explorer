//! A typed tree for self-describing RAGE metadata. Both container formats
//! this crate reads generically — the RSC7 "Meta" blocks of `.ytyp`/`.ymap`/
//! `.ymt` and the big-endian PSO sections of `.ymf`/`.pso` — carry their own
//! schema (structure sizes, member offsets, member types) and produce this
//! same tree, so one XML/JSON serializer and one set of typed readers serve
//! both. Names are hashes: the schema stores JOAAT hashes of structure and
//! member names, and a [`crate::names::NameTable`] turns them back into text.

use crate::hash::rage_joaat;
use crate::math::{Vec2, Vec3, Vec4};

/// One value in the tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum MetaValue {
    /// A null pointer, or a value that could not be read (the walker's
    /// warnings say why).
    #[default]
    Null,
    Bool(bool),
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    /// A JOAAT hash standing for a name (an archetype, a texture dictionary,
    /// a map name…).
    Hash(u32),
    Str(String),
    /// An enum member: the enum's name hash, the stored value and the member
    /// name hash when the schema knows it.
    Enum { enum_hash: u32, value: i32, name: Option<u32> },
    /// A bit set: the enum naming the bits, the stored bits, and the member
    /// name hashes of the set bits (empty when the schema names none).
    Flags { enum_hash: u32, bits: u32, names: Vec<u32> },
    /// Raw bytes: byte arrays, and any field type the walker does not decode.
    Bytes(Vec<u8>),
    Array(MetaArray),
    /// Key/value pairs of a `Map` field.
    Map(Vec<(MetaValue, MetaValue)>),
    Struct(MetaStruct),
}

/// An array field. `item_type` is the structure name hash of the elements
/// when they are structures (what the reference tool's XML writes as `itemType`);
/// `typed_items` says the elements were reached through pointers and may
/// each be a different (derived) structure, which the XML shows as
/// `<Item type="…">`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetaArray {
    pub item_type: Option<u32>,
    pub typed_items: bool,
    pub items: Vec<MetaValue>,
}

/// A structure instance: its type's name hash and its members in schema
/// order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetaStruct {
    pub type_hash: u32,
    pub fields: Vec<(u32, MetaValue)>,
}

impl MetaStruct {
    /// The member whose name hashes to `name_hash`.
    pub fn get(&self, name_hash: u32) -> Option<&MetaValue> {
        self.fields.iter().find(|(h, _)| *h == name_hash).map(|(_, v)| v)
    }

    /// The member called `name` (exact case, as Meta names are).
    pub fn field(&self, name: &str) -> Option<&MetaValue> {
        self.get(rage_joaat(name))
    }
}

impl MetaValue {
    pub fn as_struct(&self) -> Option<&MetaStruct> {
        match self {
            MetaValue::Struct(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&MetaArray> {
        match self {
            MetaValue::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The elements of an array, or nothing for any other value.
    pub fn items(&self) -> &[MetaValue] {
        self.as_array().map_or(&[], |a| a.items.as_slice())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            MetaValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A hash, or the hash of a string value (names are stored either way).
    pub fn as_hash(&self) -> Option<u32> {
        match self {
            MetaValue::Hash(h) => Some(*h),
            MetaValue::Str(s) => Some(rage_joaat(s)),
            MetaValue::U32(v) => Some(*v),
            _ => None,
        }
    }

    /// Any integer, widened.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            MetaValue::Bool(b) => i64::from(*b),
            MetaValue::I8(v) => i64::from(*v),
            MetaValue::U8(v) => i64::from(*v),
            MetaValue::I16(v) => i64::from(*v),
            MetaValue::U16(v) => i64::from(*v),
            MetaValue::I32(v) => i64::from(*v),
            MetaValue::U32(v) => i64::from(*v),
            MetaValue::I64(v) => *v,
            MetaValue::U64(v) => i64::try_from(*v).ok()?,
            MetaValue::Enum { value, .. } => i64::from(*value),
            MetaValue::Flags { bits, .. } => i64::from(*bits),
            _ => return None,
        })
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            MetaValue::Flags { bits, .. } => Some(*bits),
            _ => u32::try_from(self.as_i64()?).ok(),
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            MetaValue::F32(v) => Some(*v),
            _ => self.as_i64().map(|v| v as f32),
        }
    }

    pub fn as_vec3(&self) -> Option<Vec3> {
        match self {
            MetaValue::Vec3(v) => Some(*v),
            MetaValue::Vec4(v) => Some(Vec3::new(v.x, v.y, v.z)),
            _ => None,
        }
    }

    /// The hashes of a hash array (or of a string array's names).
    pub fn hashes(&self) -> Vec<u32> {
        self.items().iter().filter_map(MetaValue::as_hash).collect()
    }
}

/// What a generic walk produced: the tree plus everything it could not
/// decode faithfully. A walk never fails on an unsupported field; it leaves
/// bytes behind and says so here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetaDump {
    pub root: MetaValue,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_fields_are_found_by_name_or_hash() {
        let s = MetaStruct { type_hash: 1, fields: vec![(rage_joaat("name"), MetaValue::Hash(7)), (rage_joaat("flags"), MetaValue::U32(3))] };
        assert_eq!(s.field("name"), Some(&MetaValue::Hash(7)));
        assert_eq!(s.get(rage_joaat("flags")).and_then(MetaValue::as_u32), Some(3));
        assert_eq!(s.field("missing"), None);
    }

    #[test]
    fn strings_hash_like_names() {
        assert_eq!(MetaValue::Str("map1".into()).as_hash(), Some(rage_joaat("map1")));
        assert_eq!(MetaValue::Flags { enum_hash: 0, bits: 5, names: vec![] }.as_u32(), Some(5));
        assert_eq!(MetaValue::U64(u64::MAX).as_i64(), None);
    }
}
