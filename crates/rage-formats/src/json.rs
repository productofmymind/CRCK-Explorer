//! JSON for a [`MetaValue`] tree: structures become objects with a `$type`
//! member, arrays become arrays, vectors become `[x, y, z]`, hashes and
//! enum members become their names (or `hash_XXXXXXXX`), flags become
//! `{"value": bits, "flags": [names]}` and raw bytes a hex string.

use json::JsonValue;

use crate::names::NameTable;
use crate::value::MetaValue;

/// Converts the tree into a JSON value.
pub fn to_json(value: &MetaValue, names: &NameTable) -> JsonValue {
    match value {
        MetaValue::Null => JsonValue::Null,
        MetaValue::Bool(b) => JsonValue::from(*b),
        MetaValue::I8(v) => JsonValue::from(*v),
        MetaValue::U8(v) => JsonValue::from(*v),
        MetaValue::I16(v) => JsonValue::from(*v),
        MetaValue::U16(v) => JsonValue::from(*v),
        MetaValue::I32(v) => JsonValue::from(*v),
        MetaValue::U32(v) => JsonValue::from(*v),
        MetaValue::I64(v) => JsonValue::from(*v),
        MetaValue::U64(v) => JsonValue::from(*v),
        // Through f64: the json crate stores an f32 with fewer digits than it has.
        MetaValue::F32(v) => JsonValue::from(*v as f64),
        MetaValue::Vec2(v) => json::array![v.x as f64, v.y as f64],
        MetaValue::Vec3(v) => json::array![v.x as f64, v.y as f64, v.z as f64],
        MetaValue::Vec4(v) => json::array![v.x as f64, v.y as f64, v.z as f64, v.w as f64],
        MetaValue::Hash(h) => {
            if *h == 0 {
                JsonValue::Null
            } else {
                JsonValue::from(names.resolve(*h).as_ref())
            }
        }
        MetaValue::Str(s) => JsonValue::from(s.as_str()),
        MetaValue::Enum { value, name, .. } => match name {
            Some(n) => JsonValue::from(names.resolve(*n).as_ref()),
            None => JsonValue::from(*value),
        },
        MetaValue::Flags { bits, names: set, .. } => {
            let flags: Vec<JsonValue> = set.iter().map(|h| JsonValue::from(names.resolve(*h).as_ref())).collect();
            json::object! { value: *bits, flags: flags }
        }
        MetaValue::Bytes(b) => JsonValue::from(b.iter().map(|x| format!("{x:02X}")).collect::<String>()),
        MetaValue::Array(a) => JsonValue::Array(a.items.iter().map(|v| to_json(v, names)).collect()),
        MetaValue::Map(pairs) => JsonValue::Array(pairs.iter().map(|(k, v)| json::object! { key: to_json(k, names), value: to_json(v, names) }).collect()),
        MetaValue::Struct(s) => {
            let mut obj = json::object::Object::with_capacity(s.fields.len() + 1);
            obj.insert("$type", JsonValue::from(names.resolve(s.type_hash).as_ref()));
            for (hash, v) in &s.fields {
                obj.insert(&names.resolve(*hash), to_json(v, names));
            }
            JsonValue::Object(obj)
        }
    }
}

/// The inverse of [`to_json`], without a schema: objects with a `$type`
/// become structures (names or `hash_XXXXXXXX` hashed), other objects
/// with `value`/`flags` become flag sets, arrays of `{key, value}` objects
/// become maps, numbers become integers or floats, strings stay strings
/// (a writer hashes them where a name is expected).
pub fn from_json(text: &str) -> anyhow::Result<MetaValue> {
    let parsed = json::parse(text).map_err(|e| anyhow::anyhow!("malformed JSON: {e}"))?;
    Ok(value_of(&parsed))
}

fn value_of(v: &JsonValue) -> MetaValue {
    use crate::coerce::hash_of_str;
    use crate::value::{MetaArray, MetaStruct};
    match v {
        JsonValue::Null => MetaValue::Null,
        JsonValue::Boolean(b) => MetaValue::Bool(*b),
        JsonValue::Number(_) => {
            if let Some(i) = v.as_i64() {
                match i32::try_from(i) {
                    Ok(i) => MetaValue::I32(i),
                    Err(_) => MetaValue::I64(i),
                }
            } else if let Some(u) = v.as_u64() {
                MetaValue::U64(u)
            } else {
                MetaValue::F32(v.as_f64().unwrap_or(0.0) as f32)
            }
        }
        JsonValue::Short(_) | JsonValue::String(_) => MetaValue::Str(v.as_str().unwrap_or("").to_owned()),
        JsonValue::Array(items) => {
            let is_pair = |i: &JsonValue| i.is_object() && i.len() == 2 && i.has_key("key") && i.has_key("value");
            if !items.is_empty() && items.iter().all(is_pair) {
                return MetaValue::Map(items.iter().map(|p| (value_of(&p["key"]), value_of(&p["value"]))).collect());
            }
            let typed_items = items.iter().any(|i| i.is_object() && i.has_key("$type"));
            MetaValue::Array(MetaArray { item_type: None, typed_items, items: items.iter().map(value_of).collect() })
        }
        JsonValue::Object(obj) => {
            if let Some(ty) = obj.get("$type").and_then(|t| t.as_str()) {
                let fields = obj.iter().filter(|(k, _)| *k != "$type").map(|(k, v)| (hash_of_str(k), value_of(v))).collect();
                return MetaValue::Struct(MetaStruct { type_hash: hash_of_str(ty), fields });
            }
            if obj.len() == 2 && obj.get("value").is_some() && obj.get("flags").is_some() {
                let bits = obj.get("value").and_then(|b| b.as_u32()).unwrap_or(0);
                return MetaValue::Flags { enum_hash: 0, bits, names: Vec::new() };
            }
            // A structure whose type the member's schema will supply.
            let fields = obj.iter().map(|(k, v)| (hash_of_str(k), value_of(v))).collect();
            MetaValue::Struct(MetaStruct { type_hash: 0, fields })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::rage_joaat;
    use crate::math::Vec3;
    use crate::value::{MetaArray, MetaStruct};

    #[test]
    fn a_small_tree_becomes_an_object() {
        let mut names = NameTable::empty();
        for n in ["CMapData", "name", "origin", "flags", "FLAG_A", "list"] {
            names.add(n);
        }
        let root = MetaValue::Struct(MetaStruct {
            type_hash: rage_joaat("CMapData"),
            fields: vec![
                (rage_joaat("name"), MetaValue::Hash(rage_joaat("unknown_name"))),
                (rage_joaat("origin"), MetaValue::Vec3(Vec3::new(1.0, 2.0, 3.0))),
                (rage_joaat("flags"), MetaValue::Flags { enum_hash: 0, bits: 1, names: vec![rage_joaat("FLAG_A")] }),
                (rage_joaat("list"), MetaValue::Array(MetaArray { item_type: None, typed_items: false, items: vec![MetaValue::U16(7), MetaValue::Null] })),
            ],
        });
        let j = to_json(&root, &names);
        assert_eq!(j["$type"].as_str(), Some("CMapData"));
        assert_eq!(j["name"].as_str(), Some(format!("hash_{:08X}", rage_joaat("unknown_name")).as_str()));
        assert_eq!(j["origin"][2], 3.0);
        assert_eq!(j["flags"]["flags"][0].as_str(), Some("FLAG_A"));
        assert_eq!(j["list"][0], 7);
        assert!(j["list"][1].is_null());
    }
}
