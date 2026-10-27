//! Reading a [`MetaValue`] as the type a schema member wants. Values that
//! came through XML or JSON are loosely typed (a hash is a `Str`, an enum
//! member is its name, a vector may be a list of numbers), so each writer
//! asks for the shape it needs and gets `None` when the value cannot be
//! read that way.
use crate::hash::rage_joaat;
use crate::value::MetaValue;

/// A name as a hash: `hash_XXXXXXXX` and `0x…` spell a hash out, an empty
/// string is 0, anything else is JOAAT-hashed.
pub(crate) fn hash_of_str(s: &str) -> u32 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    if let Some(hex) = s.strip_prefix("hash_").or_else(|| s.strip_prefix("hash_0x")) {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return v;
        }
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return v;
        }
    }
    rage_joaat(s)
}

pub(crate) fn hash_of(v: &MetaValue) -> Option<u32> {
    match v {
        MetaValue::Null => Some(0),
        MetaValue::Hash(h) => Some(*h),
        MetaValue::Str(s) => Some(hash_of_str(s)),
        MetaValue::U32(x) => Some(*x),
        MetaValue::I32(x) => Some(*x as u32),
        MetaValue::I64(x) => Some(*x as u32),
        MetaValue::U64(x) => Some(*x as u32),
        MetaValue::Enum { value, .. } => Some(*value as u32),
        _ => None,
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok().or_else(|| u64::from_str_radix(hex, 16).ok().map(|v| v as i64));
    }
    s.parse::<i64>().ok().or_else(|| s.parse::<u64>().ok().map(|v| v as i64)).or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
}

pub(crate) fn int_of(v: &MetaValue) -> Option<i64> {
    match v {
        MetaValue::Null => Some(0),
        MetaValue::Bool(b) => Some(*b as i64),
        MetaValue::I8(x) => Some(*x as i64),
        MetaValue::U8(x) => Some(*x as i64),
        MetaValue::I16(x) => Some(*x as i64),
        MetaValue::U16(x) => Some(*x as i64),
        MetaValue::I32(x) => Some(*x as i64),
        MetaValue::U32(x) => Some(*x as i64),
        MetaValue::I64(x) => Some(*x),
        MetaValue::U64(x) => Some(*x as i64),
        MetaValue::F32(f) => Some(*f as i64),
        MetaValue::Hash(h) => Some(*h as i64),
        MetaValue::Enum { value, .. } => Some(*value as i64),
        MetaValue::Flags { bits, .. } => Some(*bits as i64),
        MetaValue::Str(s) => {
            let t = s.trim();
            if t.eq_ignore_ascii_case("true") {
                Some(1)
            } else if t.eq_ignore_ascii_case("false") || t.is_empty() {
                Some(0)
            } else {
                parse_int(t)
            }
        }
        _ => None,
    }
}

pub(crate) fn float_of(v: &MetaValue) -> Option<f32> {
    match v {
        MetaValue::F32(f) => Some(*f),
        MetaValue::Str(s) => s.trim().parse::<f32>().ok(),
        MetaValue::Null => Some(0.0),
        other => int_of(other).map(|i| i as f32),
    }
}

/// `n` floats from a vector value, a list of numbers or a `x,y,z` string.
pub(crate) fn vec_of(v: &MetaValue, n: usize) -> Option<[f32; 4]> {
    let mut out = [0f32; 4];
    match v {
        MetaValue::Null => Some(out),
        MetaValue::Vec2(a) => {
            out[..2].copy_from_slice(&[a.x, a.y]);
            Some(out)
        }
        MetaValue::Vec3(a) => {
            out[..3].copy_from_slice(&[a.x, a.y, a.z]);
            Some(out)
        }
        MetaValue::Vec4(a) => {
            out = [a.x, a.y, a.z, a.w];
            Some(out)
        }
        MetaValue::Array(arr) => {
            for (i, item) in arr.items.iter().take(4).enumerate() {
                out[i] = float_of(item)?;
            }
            (arr.items.len() >= n.min(4)).then_some(out)
        }
        MetaValue::Str(s) => {
            let parts: Vec<f32> = s.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()).map(|p| p.parse::<f32>().ok()).collect::<Option<_>>()?;
            for (i, f) in parts.iter().take(4).enumerate() {
                out[i] = *f;
            }
            (parts.len() >= n.min(4)).then_some(out)
        }
        _ => None,
    }
}

pub(crate) fn str_of(v: &MetaValue) -> Option<String> {
    match v {
        MetaValue::Str(s) => Some(s.clone()),
        MetaValue::Null => Some(String::new()),
        MetaValue::Hash(0) => Some(String::new()),
        _ => None,
    }
}

/// Raw bytes from a byte value, a hex string or a list of small integers.
pub(crate) fn bytes_of(v: &MetaValue) -> Option<Vec<u8>> {
    match v {
        MetaValue::Bytes(b) => Some(b.clone()),
        MetaValue::Null => Some(Vec::new()),
        MetaValue::Str(s) => {
            let hex: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            if hex.len() % 2 != 0 {
                return None;
            }
            (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect()
        }
        MetaValue::Array(arr) => arr.items.iter().map(|i| int_of(i).map(|x| x as u8)).collect(),
        _ => None,
    }
}

/// A member name (or its `hash_` form) resolved against an enum's members.
fn member_value(name: &str, members: &[(u32, i32)]) -> Option<i32> {
    let h = hash_of_str(name);
    members.iter().find(|(n, _)| *n == h).map(|(_, v)| *v)
}

/// The stored value of an enum member: its numeric value, or its name
/// looked up in `members`.
pub(crate) fn enum_value(v: &MetaValue, members: &[(u32, i32)]) -> Option<i32> {
    match v {
        MetaValue::Enum { value, .. } => Some(*value),
        MetaValue::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Some(0);
            }
            parse_int(t).map(|i| i as i32).or_else(|| member_value(t, members))
        }
        MetaValue::Hash(h) => members.iter().find(|(n, _)| n == h).map(|(_, v)| *v).or(Some(*h as i32)),
        other => int_of(other).map(|i| i as i32),
    }
}

/// The bits of a flag set: the stored bits, or member names (comma or
/// whitespace separated) each contributing `1 << value`, as the reference tool does.
pub(crate) fn flags_bits(v: &MetaValue, members: &[(u32, i32)]) -> Option<u32> {
    match v {
        MetaValue::Flags { bits, .. } => Some(*bits),
        MetaValue::Str(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Some(0);
            }
            if let Some(i) = parse_int(t) {
                return Some(i as u32);
            }
            let mut bits = 0u32;
            for name in t.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()) {
                let value = member_value(name, members)?;
                bits |= 1u32 << (value & 31);
            }
            Some(bits)
        }
        MetaValue::Array(arr) => {
            let mut bits = 0u32;
            for item in &arr.items {
                bits |= flags_bits(item, members)?;
            }
            Some(bits)
        }
        other => int_of(other).map(|i| i as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_come_from_names_hex_or_numbers() {
        assert_eq!(hash_of_str(""), 0);
        assert_eq!(hash_of_str("hash_0000ABCD"), 0xABCD);
        assert_eq!(hash_of_str("0x10"), 0x10);
        assert_eq!(hash_of_str("prop_barrier"), rage_joaat("prop_barrier"));
        assert_eq!(hash_of(&MetaValue::Str("map1".into())), Some(rage_joaat("map1")));
        assert_eq!(hash_of(&MetaValue::U32(7)), Some(7));
    }

    #[test]
    fn enums_and_flags_resolve_names() {
        let members = [(rage_joaat("A"), 0), (rage_joaat("B"), 3)];
        assert_eq!(enum_value(&MetaValue::Str("B".into()), &members), Some(3));
        assert_eq!(enum_value(&MetaValue::Str("5".into()), &members), Some(5));
        assert_eq!(flags_bits(&MetaValue::Str("A, B".into()), &members), Some(0b1001));
        assert_eq!(flags_bits(&MetaValue::Str("".into()), &members), Some(0));
        assert_eq!(flags_bits(&MetaValue::I32(6), &members), Some(6));
        assert_eq!(flags_bits(&MetaValue::Str("Z".into()), &members), None);
    }

    #[test]
    fn vectors_and_bytes_take_several_shapes() {
        assert_eq!(vec_of(&MetaValue::Str("1, 2, 3".into()), 3).map(|v| v[2]), Some(3.0));
        let list = MetaValue::Array(crate::value::MetaArray { item_type: None, typed_items: false, items: vec![MetaValue::F32(1.0), MetaValue::I32(2)] });
        assert_eq!(vec_of(&list, 2).map(|v| v[1]), Some(2.0));
        assert_eq!(vec_of(&list, 3), None);
        assert_eq!(bytes_of(&MetaValue::Str("0A ff".into())), Some(vec![10, 255]));
        assert_eq!(int_of(&MetaValue::Str("0x10".into())), Some(16));
        assert_eq!(int_of(&MetaValue::Str("true".into())), Some(1));
    }
}
