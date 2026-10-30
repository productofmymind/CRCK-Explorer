//! The `.ycd.xml` text form of a clip dictionary, in the layout the reference tool and Sollumz
//! use, so a dictionary can go to either and come back.

use anyhow::{anyhow, bail, Context, Result};
use roxmltree::{Document, Node};

use crate::hash::rage_joaat;
use crate::ycd::*;

// ─── writing ───────────────────────────────────────────────────────────────────────────────

/// A float the way the reference tool prints one: the shortest text that reads back as the same value.
fn fl(v: f32) -> String {
    format!("{v}")
}

fn escape(s: &str) -> String {
    let mut e = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => e.push_str("&amp;"),
            '<' => e.push_str("&lt;"),
            '>' => e.push_str("&gt;"),
            '"' => e.push_str("&quot;"),
            '\'' => e.push_str("&apos;"),
            _ => e.push(c),
        }
    }
    e
}

pub fn hash_text(h: u32) -> String {
    format!("hash_{h:08X}")
}

/// A clip's hash is the hash of the short lower-case form of its name when it has one, which is
/// worth printing as the name; every other hash is printed as `hash_XXXXXXXX`.
fn clip_hash_text(hash: u32, name: &str) -> String {
    let short = name.replace('\\', "/");
    let short = short.rsplit('/').next().unwrap_or("");
    let short = short.split('.').next().unwrap_or("").to_lowercase();
    if !short.is_empty() && rage_joaat(&short) == hash {
        short
    } else {
        hash_text(hash)
    }
}

struct Out {
    s: String,
}

impl Out {
    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.s.push(' ');
        }
        self.s.push_str(text);
        self.s.push('\n');
    }
    fn value(&mut self, depth: usize, name: &str, v: impl std::fmt::Display) {
        self.line(depth, &format!("<{name} value=\"{v}\" />"));
    }
    fn text(&mut self, depth: usize, name: &str, t: &str) {
        self.line(depth, &format!("<{name}>{}</{name}>", escape(t)));
    }
    fn open(&mut self, depth: usize, name: &str) {
        self.line(depth, &format!("<{name}>"));
    }
    fn close(&mut self, depth: usize, name: &str) {
        self.line(depth, &format!("</{name}>"));
    }
    /// Numbers ten to a line, single spaces, as the readers split on.
    fn numbers<T: std::fmt::Display>(&mut self, depth: usize, name: &str, items: &[T]) {
        self.open(depth, name);
        for row in items.chunks(10) {
            let line: Vec<String> = row.iter().map(|v| v.to_string()).collect();
            self.line(depth + 1, &line.join(" "));
        }
        self.close(depth, name);
    }
}

pub fn to_xml(d: &ClipDictionary) -> String {
    let mut o = Out { s: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n") };
    o.open(0, "ClipDictionary");
    write_list(&mut o, 1, "Clips", &d.clips, write_clip);
    write_list(&mut o, 1, "Animations", &d.animations, write_animation);
    o.close(0, "ClipDictionary");
    o.s
}

fn write_list<T>(o: &mut Out, depth: usize, name: &str, items: &[T], each: impl Fn(&mut Out, usize, &T)) {
    if items.is_empty() {
        o.line(depth, &format!("<{name} />"));
        return;
    }
    o.open(depth, name);
    for item in items {
        o.open(depth + 1, "Item");
        each(o, depth + 2, item);
        o.close(depth + 1, "Item");
    }
    o.close(depth, name);
}

fn write_clip(o: &mut Out, d: usize, c: &Clip) {
    o.text(d, "Hash", &clip_hash_text(c.hash, &c.name));
    o.text(d, "Name", &c.name);
    o.value(d, "Type", if matches!(c.kind, ClipKind::Animation(_)) { "Animation" } else { "AnimationList" });
    o.value(d, "Unknown30", c.unknown30);
    write_list(o, d, "Tags", &c.tags, |o, d, t| {
        write_property(o, d, &t.property);
        o.value(d, "StartPhase", fl(t.start_phase));
        o.value(d, "EndPhase", fl(t.end_phase));
    });
    write_list(o, d, "Properties", &c.properties, write_property);
    match &c.kind {
        ClipKind::Animation(a) => {
            o.text(d, "AnimationHash", &hash_text(a.animation));
            o.value(d, "StartTime", fl(a.start));
            o.value(d, "EndTime", fl(a.end));
            o.value(d, "Rate", fl(a.rate));
        }
        ClipKind::List { duration, animations } => {
            o.value(d, "Duration", fl(*duration));
            write_list(o, d, "Animations", animations, |o, d, a| {
                o.text(d, "AnimationHash", &hash_text(a.animation));
                o.value(d, "StartTime", fl(a.start));
                o.value(d, "EndTime", fl(a.end));
                o.value(d, "Rate", fl(a.rate));
            });
        }
    }
}

fn write_property(o: &mut Out, d: usize, p: &Property) {
    o.text(d, "NameHash", &hash_text(p.name));
    o.text(d, "UnkHash", &hash_text(p.unk));
    write_list(o, d, "Attributes", &p.attributes, |o, d, a| {
        o.text(d, "NameHash", &hash_text(a.name));
        let (ty, _) = attr_type(&a.value);
        o.value(d, "Type", ty);
        match &a.value {
            AttrValue::Float(v) => o.value(d, "Value", fl(*v)),
            AttrValue::Int(v) => o.value(d, "Value", v),
            AttrValue::Bool(v) => o.value(d, "Value", v),
            AttrValue::String(v) => o.text(d, "Value", v),
            AttrValue::Vector3 { value, unknown2c } => {
                o.line(d, &format!("<Value x=\"{}\" y=\"{}\" z=\"{}\" />", fl(value[0]), fl(value[1]), fl(value[2])));
                o.value(d, "Unknown2C", fl(*unknown2c));
            }
            AttrValue::Vector4(v) => o.line(
                d,
                &format!("<Value x=\"{}\" y=\"{}\" z=\"{}\" w=\"{}\" />", fl(v[0]), fl(v[1]), fl(v[2]), fl(v[3])),
            ),
            AttrValue::HashString(v) => o.text(d, "Value", &hash_text(*v)),
        }
    });
}

/// The name an attribute type has in the XML, and its number in the binary.
pub(crate) fn attr_type(v: &AttrValue) -> (&'static str, u8) {
    match v {
        AttrValue::Float(_) => ("Float", 1),
        AttrValue::Int(_) => ("Int", 2),
        AttrValue::Bool(_) => ("Bool", 3),
        AttrValue::String(_) => ("String", 4),
        AttrValue::Vector3 { .. } => ("Vector3", 6),
        AttrValue::Vector4(_) => ("Vector4", 8),
        AttrValue::HashString(_) => ("HashString", 12),
    }
}

fn write_animation(o: &mut Out, d: usize, a: &Animation) {
    o.text(d, "Hash", &hash_text(a.hash));
    o.value(d, "Unknown10", a.unknown10);
    o.value(d, "FrameCount", a.frames);
    o.value(d, "SequenceFrameLimit", a.sequence_frame_limit);
    o.value(d, "Duration", fl(a.duration));
    o.text(d, "Unknown1C", &hash_text(a.unknown1c));
    write_list(o, d, "BoneIds", &a.bone_ids, |o, d, b| {
        o.value(d, "BoneId", b.id);
        o.value(d, "Track", b.track);
        o.value(d, "Unk0", b.unk0);
    });
    write_list(o, d, "Sequences", &a.sequences, |o, d, s| {
        o.text(d, "Hash", &hash_text(s.hash));
        o.value(d, "FrameCount", s.frame_count);
        write_list(o, d, "SequenceData", &s.data, |o, d, channels| {
            write_list(o, d, "Channels", channels, write_channel);
        });
    });
}

fn write_channel(o: &mut Out, d: usize, c: &Channel) {
    o.value(d, "Type", c.type_name());
    match c {
        Channel::StaticQuaternion(q) => o.line(
            d,
            &format!("<Value x=\"{}\" y=\"{}\" z=\"{}\" w=\"{}\" />", fl(q[0]), fl(q[1]), fl(q[2]), fl(q[3])),
        ),
        Channel::StaticVector3(v) => o.line(d, &format!("<Value x=\"{}\" y=\"{}\" z=\"{}\" />", fl(v[0]), fl(v[1]), fl(v[2]))),
        Channel::StaticFloat(v) => o.value(d, "Value", fl(*v)),
        Channel::RawFloat(values) => o.numbers(d, "Values", &values.iter().map(|v| fl(*v)).collect::<Vec<_>>()),
        Channel::QuantizeFloat { quantum, offset, values } => {
            o.value(d, "Quantum", fl(*quantum));
            o.value(d, "Offset", fl(*offset));
            o.numbers(d, "Values", &values.iter().map(|v| fl(*v)).collect::<Vec<_>>());
        }
        Channel::IndirectQuantizeFloat { quantum, offset, values, frames } => {
            o.value(d, "Quantum", fl(*quantum));
            o.value(d, "Offset", fl(*offset));
            o.numbers(d, "Values", &values.iter().map(|v| fl(*v)).collect::<Vec<_>>());
            o.numbers(d, "Frames", frames);
        }
        Channel::CachedQuaternion { quat_index, .. } => o.value(d, "QuatIndex", quat_index),
    }
}

// ─── reading ───────────────────────────────────────────────────────────────────────────────

/// A hash as the XML spells it: `hash_XXXXXXXX`, or a name hashed the way the game does.
pub fn parse_hash(s: &str) -> Result<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("hash_") {
        return u32::from_str_radix(hex, 16).with_context(|| format!("'{s}' is not a hash"));
    }
    Ok(rage_joaat(&s.to_lowercase()))
}

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn attr<'a>(n: Node<'a, '_>, name: &str, attribute: &str) -> Result<&'a str> {
    child(n, name)
        .and_then(|c| c.attribute(attribute))
        .ok_or_else(|| anyhow!("<{}> has no <{name} {attribute}=…>", n.tag_name().name()))
}

fn val<T: std::str::FromStr>(n: Node, name: &str) -> Result<T> {
    let s = attr(n, name, "value")?;
    s.trim().parse().map_err(|_| anyhow!("<{name} value=\"{s}\" /> is not a valid number"))
}

fn val_or<T: std::str::FromStr>(n: Node, name: &str, default: T) -> Result<T> {
    if child(n, name).is_some() { val(n, name) } else { Ok(default) }
}

fn text(n: Node, name: &str) -> Result<String> {
    child(n, name)
        .map(|c| c.text().unwrap_or("").trim().to_string())
        .ok_or_else(|| anyhow!("<{}> has no <{name}>", n.tag_name().name()))
}

fn hash_of(n: Node, name: &str) -> Result<u32> {
    parse_hash(&text(n, name)?)
}

fn items<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Vec<Node<'a, 'b>> {
    child(n, name)
        .map(|l| l.children().filter(|c| c.is_element() && c.tag_name().name() == "Item").collect())
        .unwrap_or_default()
}

fn numbers<T: std::str::FromStr>(n: Node, name: &str) -> Result<Vec<T>> {
    let t = child(n, name).and_then(|c| c.text()).unwrap_or("");
    t.split_whitespace()
        .map(|w| w.parse::<T>().map_err(|_| anyhow!("'{w}' in <{name}> is not a valid number")))
        .collect()
}

fn vec_attrs(n: Node, name: &str, axes: &[&str]) -> Result<Vec<f32>> {
    axes.iter().map(|a| attr(n, name, a)?.trim().parse::<f32>().map_err(|_| anyhow!("<{name} {a}=…> is not a number"))).collect()
}

pub fn from_xml(xml: &str) -> Result<ClipDictionary> {
    let doc = Document::parse(xml).context("not valid XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "ClipDictionary" {
        bail!("the root element is <{}>, not <ClipDictionary>", root.tag_name().name());
    }
    let clips = items(root, "Clips").into_iter().enumerate().map(|(i, n)| read_clip(n).with_context(|| format!("clip {i}"))).collect::<Result<_>>()?;
    let animations =
        items(root, "Animations").into_iter().enumerate().map(|(i, n)| read_animation(n).with_context(|| format!("animation {i}"))).collect::<Result<_>>()?;
    Ok(ClipDictionary { clips, animations })
}

fn read_clip_animation(n: Node) -> Result<ClipAnimation> {
    Ok(ClipAnimation { animation: hash_of(n, "AnimationHash")?, start: val(n, "StartTime")?, end: val(n, "EndTime")?, rate: val(n, "Rate")? })
}

fn read_clip(n: Node) -> Result<Clip> {
    let name = text(n, "Name").unwrap_or_default();
    let kind = match attr(n, "Type", "value")? {
        "Animation" => ClipKind::Animation(read_clip_animation(n)?),
        "AnimationList" => ClipKind::List {
            duration: val(n, "Duration")?,
            animations: items(n, "Animations").into_iter().map(read_clip_animation).collect::<Result<_>>()?,
        },
        other => bail!("unknown clip type '{other}'"),
    };
    Ok(Clip {
        hash: hash_of(n, "Hash")?,
        name,
        unknown30: val_or(n, "Unknown30", 0)?,
        tags: items(n, "Tags")
            .into_iter()
            .map(|t| Ok(Tag { property: read_property(t)?, start_phase: val(t, "StartPhase")?, end_phase: val(t, "EndPhase")? }))
            .collect::<Result<_>>()?,
        properties: items(n, "Properties").into_iter().map(read_property).collect::<Result<_>>()?,
        kind,
    })
}

fn read_property(n: Node) -> Result<Property> {
    let attributes = items(n, "Attributes")
        .into_iter()
        .map(|a| {
            let value = match attr(a, "Type", "value")? {
                "Float" => AttrValue::Float(val(a, "Value")?),
                "Int" => AttrValue::Int(val(a, "Value")?),
                "Bool" => AttrValue::Bool(val(a, "Value")?),
                "String" => AttrValue::String(text(a, "Value").unwrap_or_default()),
                "Vector3" => {
                    let v = vec_attrs(a, "Value", &["x", "y", "z"])?;
                    AttrValue::Vector3 { value: [v[0], v[1], v[2]], unknown2c: val_or(a, "Unknown2C", 0.0)? }
                }
                "Vector4" => {
                    let v = vec_attrs(a, "Value", &["x", "y", "z", "w"])?;
                    AttrValue::Vector4([v[0], v[1], v[2], v[3]])
                }
                "HashString" => AttrValue::HashString(hash_of(a, "Value")?),
                other => bail!("unknown attribute type '{other}'"),
            };
            Ok(Attribute { name: hash_of(a, "NameHash")?, value })
        })
        .collect::<Result<_>>()?;
    Ok(Property { name: hash_of(n, "NameHash")?, unk: hash_of(n, "UnkHash").unwrap_or(0), attributes })
}

fn read_animation(n: Node) -> Result<Animation> {
    let bone_ids = items(n, "BoneIds")
        .into_iter()
        .map(|b| Ok(BoneId { id: val(b, "BoneId")?, track: val(b, "Track")?, unk0: val_or(b, "Unk0", 0)? }))
        .collect::<Result<Vec<_>>>()?;
    let sequences = items(n, "Sequences")
        .into_iter()
        .map(|s| {
            let data = items(s, "SequenceData")
                .into_iter()
                .map(|sd| items(sd, "Channels").into_iter().map(read_channel).collect::<Result<Vec<_>>>())
                .collect::<Result<Vec<_>>>()?;
            Ok(Sequence { hash: hash_of(s, "Hash").unwrap_or(0), frame_count: val(s, "FrameCount")?, data })
        })
        .collect::<Result<_>>()?;
    Ok(Animation {
        hash: hash_of(n, "Hash")?,
        unknown10: val_or(n, "Unknown10", 0)?,
        frames: val(n, "FrameCount")?,
        sequence_frame_limit: val(n, "SequenceFrameLimit")?,
        duration: val(n, "Duration")?,
        unknown1c: hash_of(n, "Unknown1C").unwrap_or(0),
        bone_ids,
        sequences,
    })
}

fn read_channel(n: Node) -> Result<Channel> {
    Ok(match attr(n, "Type", "value")? {
        "StaticQuaternion" => {
            let v = vec_attrs(n, "Value", &["x", "y", "z", "w"])?;
            Channel::StaticQuaternion([v[0], v[1], v[2], v[3]])
        }
        "StaticVector3" => {
            let v = vec_attrs(n, "Value", &["x", "y", "z"])?;
            Channel::StaticVector3([v[0], v[1], v[2]])
        }
        "StaticFloat" => Channel::StaticFloat(val(n, "Value")?),
        "RawFloat" => Channel::RawFloat(numbers(n, "Values")?),
        // a LinearFloat in XML carries the same fields as a QuantizeFloat and is stored as one
        "QuantizeFloat" | "LinearFloat" => {
            Channel::QuantizeFloat { quantum: val(n, "Quantum")?, offset: val(n, "Offset")?, values: numbers(n, "Values")? }
        }
        "IndirectQuantizeFloat" => Channel::IndirectQuantizeFloat {
            quantum: val(n, "Quantum")?,
            offset: val(n, "Offset")?,
            values: numbers(n, "Values")?,
            frames: numbers(n, "Frames")?,
        },
        ty @ ("CachedQuaternion1" | "CachedQuaternion2") => {
            Channel::CachedQuaternion { kind: if ty.ends_with('1') { 1 } else { 2 }, quat_index: val(n, "QuatIndex")? }
        }
        other => bail!("unknown channel type '{other}'"),
    })
}
