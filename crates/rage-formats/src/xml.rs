//! XML for a [`MetaValue`] tree, in the layout the reference tool's exporters use
//! so that a dump can be diffed against one: scalars as `<name value="…"/>`,
//! strings and hashes as `<name>text</name>`, vectors as `<name x= y= z=/>`,
//! structures as nested elements and arrays as `<Item>` children (with a
//! `type` attribute when the elements were reached through pointers and
//! may differ in type).

use std::fmt::Write;

use anyhow::{Context, Result};

#[cfg(test)]
use crate::hash::rage_joaat;
use crate::math::{Vec2, Vec3, Vec4};
use crate::names::NameTable;
use crate::value::{MetaArray, MetaStruct, MetaValue};

/// Serializes `root` as a complete XML document.
pub fn to_xml(root: &MetaValue, names: &NameTable) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    match root {
        MetaValue::Struct(s) => {
            let name = names.resolve(s.type_hash).into_owned();
            write_struct_body(&mut out, 0, &name, None, &s.fields, names);
        }
        other => write_member(&mut out, 0, "value", other, names),
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn escape(s: &str) -> String {
    let mut e = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => e.push_str("&amp;"),
            '<' => e.push_str("&lt;"),
            '>' => e.push_str("&gt;"),
            '"' => e.push_str("&quot;"),
            _ => e.push(c),
        }
    }
    e
}

/// A float the way the reference tool prints one: shortest round-trip form, with
/// no exponent for the magnitudes map data uses.
pub fn float(v: f32) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn write_struct_body(out: &mut String, depth: usize, tag: &str, type_attr: Option<&str>, fields: &[(u32, MetaValue)], names: &NameTable) {
    indent(out, depth);
    match type_attr {
        Some(t) => {
            let _ = writeln!(out, "<{tag} type=\"{}\">", escape(t));
        }
        None => {
            let _ = writeln!(out, "<{tag}>");
        }
    }
    for (hash, value) in fields {
        let name = names.resolve(*hash).into_owned();
        write_member(out, depth + 1, &name, value, names);
    }
    indent(out, depth);
    let _ = writeln!(out, "</{tag}>");
}

fn write_member(out: &mut String, depth: usize, name: &str, value: &MetaValue, names: &NameTable) {
    match value {
        MetaValue::Null => {
            indent(out, depth);
            let _ = writeln!(out, "<{name}/>");
        }
        MetaValue::Bool(b) => value_tag(out, depth, name, if *b { "true" } else { "false" }),
        MetaValue::I8(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::U8(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::I16(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::U16(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::I32(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::U32(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::I64(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::U64(v) => value_tag(out, depth, name, &v.to_string()),
        MetaValue::F32(v) => value_tag(out, depth, name, &float(*v)),
        MetaValue::Vec2(v) => {
            indent(out, depth);
            let _ = writeln!(out, "<{name} x=\"{}\" y=\"{}\"/>", float(v.x), float(v.y));
        }
        MetaValue::Vec3(v) => {
            indent(out, depth);
            let _ = writeln!(out, "<{name} x=\"{}\" y=\"{}\" z=\"{}\"/>", float(v.x), float(v.y), float(v.z));
        }
        MetaValue::Vec4(v) => {
            indent(out, depth);
            let _ = writeln!(out, "<{name} x=\"{}\" y=\"{}\" z=\"{}\" w=\"{}\"/>", float(v.x), float(v.y), float(v.z), float(v.w));
        }
        MetaValue::Hash(h) => {
            let text = if *h == 0 { String::new() } else { names.resolve(*h).into_owned() };
            string_tag(out, depth, name, &text)
        }
        MetaValue::Str(s) => string_tag(out, depth, name, s),
        MetaValue::Enum { value, name: member, .. } => match member {
            Some(m) => string_tag(out, depth, name, &names.resolve(*m)),
            None => value_tag(out, depth, name, &value.to_string()),
        },
        MetaValue::Flags { bits, names: set, .. } => {
            if set.is_empty() {
                if *bits == 0 {
                    indent(out, depth);
                    let _ = writeln!(out, "<{name}/>");
                } else {
                    value_tag(out, depth, name, &bits.to_string());
                }
            } else {
                let joined: Vec<String> = set.iter().map(|h| names.resolve(*h).into_owned()).collect();
                string_tag(out, depth, name, &joined.join(", "));
            }
        }
        MetaValue::Bytes(b) => {
            if b.is_empty() {
                indent(out, depth);
                let _ = writeln!(out, "<{name} itemType=\"ByteArray\"/>");
            } else {
                indent(out, depth);
                let _ = writeln!(out, "<{name} itemType=\"ByteArray\">");
                for row in b.chunks(32) {
                    indent(out, depth + 1);
                    let hex: Vec<String> = row.iter().map(|x| format!("{x:02X}")).collect();
                    out.push_str(&hex.join(" "));
                    out.push('\n');
                }
                indent(out, depth);
                let _ = writeln!(out, "</{name}>");
            }
        }
        MetaValue::Array(a) => write_array(out, depth, name, a, names),
        MetaValue::Map(pairs) => {
            if pairs.is_empty() {
                indent(out, depth);
                let _ = writeln!(out, "<{name}/>");
                return;
            }
            indent(out, depth);
            let _ = writeln!(out, "<{name}>");
            for (k, v) in pairs {
                indent(out, depth + 1);
                out.push_str("<Item>\n");
                write_member(out, depth + 2, "key", k, names);
                write_member(out, depth + 2, "value", v, names);
                indent(out, depth + 1);
                out.push_str("</Item>\n");
            }
            indent(out, depth);
            let _ = writeln!(out, "</{name}>");
        }
        MetaValue::Struct(s) => write_struct_body(out, depth, name, None, &s.fields, names),
    }
}

fn value_tag(out: &mut String, depth: usize, name: &str, v: &str) {
    indent(out, depth);
    let _ = writeln!(out, "<{name} value=\"{}\"/>", escape(v));
}

fn string_tag(out: &mut String, depth: usize, name: &str, v: &str) {
    indent(out, depth);
    if v.is_empty() {
        let _ = writeln!(out, "<{name}/>");
    } else {
        let _ = writeln!(out, "<{name}>{}</{name}>", escape(v));
    }
}

/// The `itemType` attribute of a raw array, from its first element.
fn raw_item_type(a: &MetaArray) -> Option<&'static str> {
    Some(match a.items.first()? {
        MetaValue::Bool(_) => "boolean",
        MetaValue::I8(_) => "sbyte",
        MetaValue::U8(_) => "byte",
        MetaValue::I16(_) => "short",
        MetaValue::U16(_) => "ushort",
        MetaValue::I32(_) => "int",
        MetaValue::U32(_) => "uint",
        MetaValue::I64(_) | MetaValue::U64(_) => "long",
        MetaValue::F32(_) => "float",
        MetaValue::Vec2(_) => "Vector2",
        MetaValue::Vec3(_) => "Vector3",
        MetaValue::Vec4(_) => "Vector4",
        MetaValue::Hash(_) | MetaValue::Str(_) => "Hash",
        MetaValue::Enum { .. } => "enum",
        _ => return None,
    })
}

fn write_array(out: &mut String, depth: usize, name: &str, a: &MetaArray, names: &NameTable) {
    let item_type = match a.item_type {
        Some(h) => Some(names.resolve(h).into_owned()),
        None => raw_item_type(a).map(str::to_owned),
    };
    if a.items.is_empty() {
        indent(out, depth);
        match &item_type {
            Some(t) => {
                let _ = writeln!(out, "<{name} itemType=\"{}\"/>", escape(t));
            }
            None => {
                let _ = writeln!(out, "<{name}/>");
            }
        }
        return;
    }
    indent(out, depth);
    match &item_type {
        Some(t) => {
            let _ = writeln!(out, "<{name} itemType=\"{}\">", escape(t));
        }
        None => {
            let _ = writeln!(out, "<{name}>");
        }
    }
    let numeric = a.items.iter().all(|v| matches!(v, MetaValue::Bool(_) | MetaValue::I8(_) | MetaValue::U8(_) | MetaValue::I16(_) | MetaValue::U16(_) | MetaValue::I32(_) | MetaValue::U32(_) | MetaValue::I64(_) | MetaValue::U64(_) | MetaValue::F32(_)));
    if numeric {
        // Raw numbers go in rows, as the reference tool writes them.
        for row in a.items.chunks(10) {
            indent(out, depth + 1);
            let cells: Vec<String> = row
                .iter()
                .map(|v| match v {
                    MetaValue::F32(f) => float(*f),
                    MetaValue::Bool(b) => u8::from(*b).to_string(),
                    other => other.as_i64().map_or_else(String::new, |i| i.to_string()),
                })
                .collect();
            out.push_str(&cells.join(" "));
            out.push('\n');
        }
    } else {
        for item in &a.items {
            match item {
                MetaValue::Struct(s) => {
                    let type_attr = a.typed_items.then(|| names.resolve(s.type_hash).into_owned());
                    write_struct_body(out, depth + 1, "Item", type_attr.as_deref(), &s.fields, names);
                }
                other => write_member(out, depth + 1, "Item", other, names),
            }
        }
    }
    indent(out, depth);
    let _ = writeln!(out, "</{name}>");
}

/// Reads a document in the layout [`to_xml`] writes (the reference tool's) back
/// into a tree. Element names are hashed, so the result is what a binary
/// file would have produced, minus the type information the schema would
/// carry: numbers become `I32`/`F32`, text becomes `Str` (which hashes like
/// a name where a hash is expected), `<Item>` lists become arrays.
pub fn from_xml(text: &str) -> Result<MetaValue> {
    let doc = roxmltree::Document::parse(text).context("malformed XML")?;
    Ok(element(doc.root_element()))
}

fn element(node: roxmltree::Node<'_, '_>) -> MetaValue {
    let attr = |n: &str| node.attribute(n);
    if let Some(v) = attr("value") {
        return scalar(v);
    }
    if let (Some(x), Some(y)) = (attr("x"), attr("y")) {
        let f = |s: &str| s.trim().parse::<f32>().unwrap_or(0.0);
        return match (attr("z"), attr("w")) {
            (Some(z), Some(w)) => MetaValue::Vec4(Vec4::new(f(x), f(y), f(z), f(w))),
            (Some(z), None) => MetaValue::Vec3(Vec3::new(f(x), f(y), f(z))),
            _ => MetaValue::Vec2(Vec2::new(f(x), f(y))),
        };
    }
    let children: Vec<_> = node.children().filter(|c| c.is_element()).collect();
    // Names the dump could not resolve come back as `hash_XXXXXXXX`.
    let item_type = attr("itemType").map(crate::coerce::hash_of_str);
    if children.is_empty() {
        let text = node.text().map(str::trim).unwrap_or("");
        if let Some(t) = attr("itemType") {
            // A raw array: numbers or hex bytes in rows.
            if t == "ByteArray" {
                let bytes = text.split_whitespace().filter_map(|h| u8::from_str_radix(h, 16).ok()).collect();
                return MetaValue::Bytes(bytes);
            }
            let items = text.split_whitespace().map(scalar).collect();
            return MetaValue::Array(MetaArray { item_type: None, typed_items: false, items });
        }
        if text.is_empty() && attr("type").is_none() && node.tag_name().name() != "Item" && is_plural_list(node) {
            return MetaValue::Array(MetaArray::default());
        }
        return MetaValue::Str(text.to_owned());
    }
    if children.iter().all(|c| c.tag_name().name() == "Item") {
        let typed_items = children.iter().any(|c| c.has_attribute("type"));
        let items = children.into_iter().map(element).collect();
        return MetaValue::Array(MetaArray { item_type, typed_items, items });
    }
    let type_hash = crate::coerce::hash_of_str(attr("type").unwrap_or(node.tag_name().name()));
    let fields = children.into_iter().map(|c| (crate::coerce::hash_of_str(c.tag_name().name()), element(c))).collect();
    MetaValue::Struct(MetaStruct { type_hash, fields })
}

/// Whether an empty element is an empty list rather than an empty string.
/// Without a schema only hint is the name, so this lists the members
/// of the map formats that only ever hold items.
fn is_plural_list(node: roxmltree::Node<'_, '_>) -> bool {
    matches!(
        node.tag_name().name(),
        "entities" | "extensions" | "archetypes" | "rooms" | "portals" | "entitySets" | "attachedObjects" | "locations" | "containerLods" | "boxOccluders" | "occludeModels" | "physicsDictionaries" | "timeCycleModifiers" | "carGenerators" | "instances" | "corners" | "MapDataGroups" | "HDTxdBindingArray" | "imapDependencies" | "imapDependencies_2" | "itypDependencies_2" | "Interiors" | "Bounds" | "WeatherTypes" | "itypDepArray" | "dependencies" | "compositeEntityTypes" | "defaultEntitySets" | "Effects"
    )
}

fn scalar(v: &str) -> MetaValue {
    let v = v.trim();
    match v {
        "true" => return MetaValue::Bool(true),
        "false" => return MetaValue::Bool(false),
        _ => {}
    }
    if let Ok(i) = v.parse::<i64>() {
        return match i32::try_from(i) {
            Ok(i) => MetaValue::I32(i),
            Err(_) => MetaValue::I64(i),
        };
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        if let Ok(u) = u32::from_str_radix(hex, 16) {
            return MetaValue::U32(u);
        }
    }
    match v.parse::<f32>() {
        Ok(f) => MetaValue::F32(f),
        Err(_) => MetaValue::Str(v.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_tree_prints_in_reference_layout() {
        let mut names = NameTable::empty();
        for n in ["CMapData", "name", "flags", "streamingExtentsMin", "entities", "CEntityDef", "archetypeName", "lodDist", "prop_a", "map1", "physicsDictionaries", "lodLevel", "LODTYPES_DEPTH_HD"] {
            names.add(n);
        }
        let entity = MetaStruct {
            type_hash: rage_joaat("CEntityDef"),
            fields: vec![
                (rage_joaat("archetypeName"), MetaValue::Hash(rage_joaat("prop_a"))),
                (rage_joaat("lodDist"), MetaValue::F32(120.5)),
                (rage_joaat("lodLevel"), MetaValue::Enum { enum_hash: 1, value: 0, name: Some(rage_joaat("LODTYPES_DEPTH_HD")) }),
            ],
        };
        let root = MetaValue::Struct(MetaStruct {
            type_hash: rage_joaat("CMapData"),
            fields: vec![
                (rage_joaat("name"), MetaValue::Hash(rage_joaat("map1"))),
                (rage_joaat("flags"), MetaValue::U32(32)),
                (rage_joaat("streamingExtentsMin"), MetaValue::Vec3(Vec3::new(-1.5, 2.0, 3.25))),
                (rage_joaat("entities"), MetaValue::Array(MetaArray { item_type: Some(rage_joaat("CEntityDef")), typed_items: true, items: vec![MetaValue::Struct(entity)] })),
                (rage_joaat("physicsDictionaries"), MetaValue::Array(MetaArray { item_type: None, typed_items: false, items: vec![MetaValue::Hash(0xABCD)] })),
            ],
        });
        let xml = to_xml(&root, &names);
        let expected = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<CMapData>
  <name>map1</name>
  <flags value=\"32\"/>
  <streamingExtentsMin x=\"-1.5\" y=\"2\" z=\"3.25\"/>
  <entities itemType=\"CEntityDef\">
    <Item type=\"CEntityDef\">
      <archetypeName>prop_a</archetypeName>
      <lodDist value=\"120.5\"/>
      <lodLevel>LODTYPES_DEPTH_HD</lodLevel>
    </Item>
  </entities>
  <physicsDictionaries itemType=\"Hash\">
    <Item>hash_0000ABCD</Item>
  </physicsDictionaries>
</CMapData>
";
        assert_eq!(xml, expected);
    }

    #[test]
    fn text_is_escaped_and_empties_self_close() {
        let names = NameTable::empty();
        let root = MetaValue::Struct(MetaStruct {
            type_hash: 5,
            fields: vec![(1, MetaValue::Str("a<b&\"c\"".into())), (2, MetaValue::Str(String::new())), (3, MetaValue::Array(MetaArray::default())), (4, MetaValue::Hash(0))],
        });
        let xml = to_xml(&root, &names);
        assert!(xml.contains("<hash_00000001>a&lt;b&amp;&quot;c&quot;</hash_00000001>"), "{xml}");
        assert!(xml.contains("<hash_00000002/>"), "{xml}");
        assert!(xml.contains("<hash_00000003/>"), "{xml}");
        assert!(xml.contains("<hash_00000004/>"), "{xml}");
    }

    #[test]
    fn xml_reads_back_into_an_equivalent_tree() {
        let text = "<CMapData>
  <name>map1</name>
  <flags value=\"32\"/>
  <streamingExtentsMin x=\"-1.5\" y=\"2\" z=\"3.25\"/>
  <entities itemType=\"CEntityDef\">
    <Item type=\"CEntityDef\">
      <archetypeName>prop_a</archetypeName>
      <lodDist value=\"120.5\"/>
      <extensions/>
    </Item>
  </entities>
  <physicsDictionaries/>
  <block>
    <version value=\"0\"/>
  </block>
</CMapData>";
        let v = from_xml(text).unwrap();
        let root = v.as_struct().unwrap();
        assert_eq!(root.type_hash, rage_joaat("CMapData"));
        assert_eq!(root.field("name").and_then(MetaValue::as_hash), Some(rage_joaat("map1")));
        assert_eq!(root.field("flags"), Some(&MetaValue::I32(32)));
        assert_eq!(root.field("streamingExtentsMin"), Some(&MetaValue::Vec3(Vec3::new(-1.5, 2.0, 3.25))));
        let entities = root.field("entities").unwrap().as_array().unwrap();
        assert!(entities.typed_items);
        let e = entities.items[0].as_struct().unwrap();
        assert_eq!(e.type_hash, rage_joaat("CEntityDef"));
        assert_eq!(e.field("lodDist"), Some(&MetaValue::F32(120.5)));
        assert_eq!(e.field("extensions"), Some(&MetaValue::Array(MetaArray::default())));
        assert_eq!(root.field("physicsDictionaries").map(MetaValue::items).map(<[MetaValue]>::len), Some(0));
        assert_eq!(root.field("block").unwrap().as_struct().unwrap().field("version"), Some(&MetaValue::I32(0)));
    }

    #[test]
    fn floats_print_shortest() {
        assert_eq!(float(1.0), "1");
        assert_eq!(float(-0.5), "-0.5");
        assert_eq!(float(120.25), "120.25");
        assert_eq!(float(0.1), "0.1");
    }
}
