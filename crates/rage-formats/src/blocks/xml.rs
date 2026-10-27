//! The XML toolkit every block type uses to write and read the reference tool's
//! format: `MetaXml`'s tag writers (`OpenTag`, `ValueTag`, `StringTag`,
//! `WriteRawArray`), `FloatUtil`'s shortest round-tripping floats, `Xml`'s
//! raw-array parsers and `XmlMeta.GetHash`'s hash strings.

use crate::math::{Vec3, Vec4};
use crate::names::NameTable;
use crate::rage_joaat;

pub use crate::xml::float;

/// An indenting writer producing the reference tool-shaped XML (two spaces per level).
pub struct XmlOut {
    pub out: String,
    pub indent: usize,
}

impl Default for XmlOut {
    fn default() -> Self { Self::new() }
}

impl XmlOut {
    /// A writer whose buffer already holds the `<?xml ... ?>` header line.
    pub fn new() -> Self {
        Self { out: "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".to_owned(), indent: 0 }
    }
    fn pad(&mut self) {
        for _ in 0..self.indent { self.out.push_str("  "); }
    }
    /// `<tag>` on its own line; the indent grows.
    pub fn open(&mut self, tag: &str) {
        self.pad();
        self.out.push_str(&format!("<{tag}>\n"));
        self.indent += 1;
    }
    /// `<tag attrs>` on its own line; the indent grows.
    pub fn open_attrs(&mut self, tag: &str, attrs: &str) {
        self.pad();
        self.out.push_str(&format!("<{tag} {attrs}>\n"));
        self.indent += 1;
    }
    /// The indent shrinks, then `</tag>`.
    pub fn close(&mut self, tag: &str) {
        self.indent = self.indent.saturating_sub(1);
        self.pad();
        self.out.push_str(&format!("</{tag}>\n"));
    }
    /// `<name value="v" />`.
    pub fn value(&mut self, name: &str, v: impl std::fmt::Display) {
        self.pad();
        self.out.push_str(&format!("<{name} value=\"{v}\" />\n"));
    }
    /// `<name>escaped</name>`, or `<name />` when `s` is empty.
    pub fn string(&mut self, name: &str, s: &str) {
        self.pad();
        if s.is_empty() {
            self.out.push_str(&format!("<{name} />\n"));
        } else {
            self.out.push_str(&format!("<{name}>{}</{name}>\n", escape(s)));
        }
    }
    /// `<body />`.
    pub fn self_closing(&mut self, body: &str) {
        self.pad();
        self.out.push_str(&format!("<{body} />\n"));
    }
    pub fn vec3(&mut self, name: &str, v: Vec3) {
        self.self_closing(&format!("{name} x=\"{}\" y=\"{}\" z=\"{}\"", float(v.x), float(v.y), float(v.z)));
    }
    pub fn vec4(&mut self, name: &str, v: Vec4) {
        self.self_closing(&format!("{name} x=\"{}\" y=\"{}\" z=\"{}\" w=\"{}\"", float(v.x), float(v.y), float(v.z), float(v.w)));
    }
    pub fn rgb(&mut self, name: &str, r: u8, g: u8, b: u8) {
        self.self_closing(&format!("{name} r=\"{r}\" g=\"{g}\" b=\"{b}\""));
    }
    /// A raw, indented line.
    pub fn line(&mut self, text: &str) {
        self.pad();
        self.out.push_str(text);
        self.out.push('\n');
    }
    /// `MetaXml.WriteRawArray`: one line `<name>a b c</name>` when there are at
    /// most `per_row` items, else the items `per_row` to a line between the tags.
    pub fn raw_array<T>(&mut self, name: &str, items: &[T], per_row: usize, fmt: impl Fn(&T) -> String) {
        let per_row = per_row.max(1);
        if items.is_empty() {
            self.self_closing(name);
        } else if items.len() <= per_row {
            let row: Vec<String> = items.iter().map(&fmt).collect();
            self.pad();
            self.out.push_str(&format!("<{name}>{}</{name}>\n", row.join(" ")));
        } else {
            self.open(name);
            for chunk in items.chunks(per_row) {
                let row: Vec<String> = chunk.iter().map(&fmt).collect();
                self.line(&row.join(" "));
            }
            self.close(name);
        }
    }
}

/// `WriteItemArray`: `<name>` around an `<Item>` per element, `<name />` for none.
pub fn write_items<T>(x: &mut XmlOut, name: &str, list: &[T], each: impl Fn(&mut XmlOut, &T)) {
    if list.is_empty() { x.self_closing(name); return; }
    x.open(name);
    for i in list { x.open("Item"); each(x, i); x.close("Item"); }
    x.close(name);
}

/// Escape `& < > "`.
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// `""` for 0, the known name, or `hash_XXXXXXXX`.
pub fn hash_string(h: u32, names: &NameTable) -> String {
    if h == 0 {
        String::new()
    } else {
        match names.get(h) {
            Some(n) => n.to_owned(),
            None => format!("hash_{h:08X}"),
        }
    }
}

fn hash_literal(s: &str) -> Option<u32> {
    u32::from_str_radix(s.strip_prefix("hash_")?, 16).ok()
}

/// `XmlMeta.GetHash`: `""` is 0, `hash_HEX` is that hash, anything else is hashed.
pub fn parse_hash(s: &str) -> u32 {
    if s.is_empty() { return 0; }
    hash_literal(s).unwrap_or_else(|| rage_joaat(s))
}

/// Like [`parse_hash`] but hashes the lowercased text.
pub fn parse_hash_lower(s: &str) -> u32 {
    if s.is_empty() { return 0; }
    hash_literal(s).unwrap_or_else(|| rage_joaat(&s.to_lowercase()))
}

pub type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

pub fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}
/// The child's text, `""` when absent.
pub fn child_text(n: Node, name: &str) -> String {
    child(n, name).and_then(|c| c.text()).unwrap_or("").to_owned()
}
pub fn attr_str<'a>(n: Node<'a, '_>, attr: &str) -> &'a str { n.attribute(attr).unwrap_or("") }
pub fn attr_f32(n: Node, attr: &str) -> f32 { attr_str(n, attr).trim().parse().unwrap_or(0.0) }
pub fn attr_u32(n: Node, attr: &str) -> u32 { attr_str(n, attr).trim().parse().unwrap_or(0) }
pub fn attr_i32(n: Node, attr: &str) -> i32 { attr_str(n, attr).trim().parse().unwrap_or(0) }
pub fn child_attr_f32(n: Node, name: &str, attr: &str) -> f32 { child(n, name).map_or(0.0, |c| attr_f32(c, attr)) }
pub fn child_attr_u32(n: Node, name: &str, attr: &str) -> u32 { child(n, name).map_or(0, |c| attr_u32(c, attr)) }
pub fn child_attr_i32(n: Node, name: &str, attr: &str) -> i32 { child(n, name).map_or(0, |c| attr_i32(c, attr)) }
pub fn child_vec3(n: Node, name: &str) -> Vec3 {
    child(n, name).map_or_else(Vec3::default, |c| Vec3::new(attr_f32(c, "x"), attr_f32(c, "y"), attr_f32(c, "z")))
}
pub fn child_vec4(n: Node, name: &str) -> Vec4 {
    child(n, name).map_or_else(Vec4::default, |c| Vec4::new(attr_f32(c, "x"), attr_f32(c, "y"), attr_f32(c, "z"), attr_f32(c, "w")))
}
/// The `<Item>` children of `<name>`; empty when `<name>` is absent.
pub fn items<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Vec<Node<'a, 'i>> {
    child(n, name).map_or_else(Vec::new, |c| c.children().filter(|i| i.is_element() && i.tag_name().name() == "Item").collect())
}

fn text_of<'a>(n: Node<'a, '_>) -> &'a str { n.text().unwrap_or("") }
/// Whitespace-separated u16s; unparsable words are skipped.
pub fn raw_u16s(n: Node) -> Vec<u16> { text_of(n).split_whitespace().filter_map(|w| w.parse().ok()).collect() }
/// Whitespace-separated f32s; unparsable words are skipped.
pub fn raw_f32s(n: Node) -> Vec<f32> { text_of(n).split_whitespace().filter_map(|w| w.parse().ok()).collect() }

/// One comma-separated row per non-blank line; missing or bad cells are 0.
fn comma_rows<T: Copy + Default + std::str::FromStr, const N: usize>(n: Node) -> Vec<[T; N]> {
    text_of(n)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut a = [T::default(); N];
            for (slot, w) in a.iter_mut().zip(l.split(',')) { *slot = w.trim().parse().unwrap_or_default(); }
            a
        })
        .collect()
}
/// `Xml.GetRawVector3Array`: one `x, y, z` per line.
pub fn raw_vec3s(n: Node) -> Vec<Vec3> { comma_rows::<f32, 3>(n).into_iter().map(|a| Vec3::new(a[0], a[1], a[2])).collect() }
/// One `r, g, b, a` per line.
pub fn raw_rgba(n: Node) -> Vec<[u8; 4]> { comma_rows::<u8, 4>(n) }

/// IEEE half to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let man = (h & 0x3FF) as u32;
    let bits = match (exp, man) {
        (0, 0) => sign << 31,
        (0, m) => {
            // subnormal: shift the leading bit up to bit 10 and drop it
            let shift = m.leading_zeros() - 21;
            (sign << 31) | ((127 - 15 + 1 - shift) << 23) | (((m << shift) & 0x3FF) << 13)
        }
        (0x1F, m) => (sign << 31) | 0x7F80_0000 | (m << 13),
        (e, m) => (sign << 31) | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

/// f32 to IEEE half, round to nearest even; overflow goes to infinity.
pub fn f32_to_f16(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32;
    let man = b & 0x7F_FFFF;
    if exp == 0xFF {
        return sign | 0x7C00 | if man != 0 { 0x200 | (man >> 13) as u16 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F { return sign | 0x7C00; }
    if e <= 0 {
        if e < -10 { return sign; }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let mut h = (m >> shift) as u16;
        let rem = m & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        if rem > half || (rem == half && h & 1 == 1) { h += 1; }
        return sign | h;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1FFF;
    if rem > 0x1000 || (rem == 0x1000 && h & 1 == 1) { h += 1; } // a carry into the exponent is correct
    sign | h as u16
}

/// Parse an enum list like `"A, B"` into the OR of the named values.
pub fn parse_flags<T: Copy>(text: &str, names: &[(&str, T)], or: impl Fn(T, T) -> T, zero: T) -> T {
    let mut v = zero;
    for w in text.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()) {
        if let Some((_, f)) = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(w)) { v = or(v, *f); }
    }
    v
}

/// The inverse of [`parse_flags`]: the names of the set flags joined by `, `, or `none`.
pub fn format_flags<T: Copy + PartialEq>(v: T, names: &[(&str, T)], has: impl Fn(T, T) -> bool, zero: T, none: &str) -> String {
    if v == zero { return none.to_owned(); }
    let set: Vec<&str> = names.iter().filter(|(_, f)| *f != zero && has(v, *f)).map(|(n, _)| *n).collect();
    if set.is_empty() { none.to_owned() } else { set.join(", ") }
}

/// The first line at which two documents differ, both versions quoted; `None` when they are equal.
pub fn first_difference(a: &str, b: &str) -> Option<String> {
    if a == b { return None; }
    let (mut la, mut lb) = (a.lines(), b.lines());
    let mut n = 0;
    loop {
        n += 1;
        match (la.next(), lb.next()) {
            (Some(x), Some(y)) if x == y => continue,
            (x, y) => return Some(format!("line {n}: expected `{}`, read back `{}`", x.unwrap_or("<end>").trim(), y.unwrap_or("<end>").trim())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_reference_shaped_tags() {
        let mut x = XmlOut::new();
        x.open("Drawable");
        x.value("LodDistHigh", float(9999.0));
        x.string("Name", "a<b");
        x.vec3("BoundingBoxMin", Vec3::new(-1.5, 0.0, 2.0));
        x.raw_array("Data", &[1u16, 2, 3], 2, |v| v.to_string());
        x.close("Drawable");
        assert_eq!(x.out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Drawable>\n  <LodDistHigh value=\"9999\" />\n  <Name>a&lt;b</Name>\n  <BoundingBoxMin x=\"-1.5\" y=\"0\" z=\"2\" />\n  <Data>\n    1 2\n    3\n  </Data>\n</Drawable>\n");
    }
    #[test]
    fn reads_attributes_children_and_raw_arrays() {
        let doc = roxmltree::Document::parse("<R><A value=\"3\"/><V x=\"1\" y=\"2\" z=\"3\"/><L><Item><N>x</N></Item><Item/></L><D>1 2\n 3</D><P>1, 2, 3\n4, 5, 6</P></R>").unwrap();
        let r = doc.root_element();
        assert_eq!(child_attr_u32(r, "A", "value"), 3);
        assert_eq!(child_attr_f32(r, "Missing", "value"), 0.0);
        assert_eq!(child_vec3(r, "V"), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(items(r, "L").len(), 2);
        assert_eq!(raw_u16s(child(r, "D").unwrap()), vec![1, 2, 3]);
        assert_eq!(raw_vec3s(child(r, "P").unwrap())[1], Vec3::new(4.0, 5.0, 6.0));
    }
    #[test]
    fn empty_raw_array_is_self_closing() {
        let mut x = XmlOut::new();
        x.out.clear();
        x.raw_array("Data", &[] as &[u16], 4, |v| v.to_string());
        assert_eq!(x.out, "<Data />\n");
    }
    #[test]
    fn hash_strings_and_escaping() {
        let mut names = NameTable::empty();
        names.add("prop_a");
        assert_eq!(hash_string(0, &names), "");
        assert_eq!(hash_string(crate::rage_joaat("prop_a"), &names), "prop_a");
        assert_eq!(hash_string(0xABC, &names), "hash_00000ABC");
        assert_eq!(hash_string(0xDEADBEEF, &names), "hash_DEADBEEF");
        assert_eq!(escape("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
    }
    #[test]
    fn flags_parse_and_format() {
        let names = [("A", 1u32), ("B", 2u32)];
        assert_eq!(parse_flags("A, B", &names, |a, b| a | b, 0), 3);
        assert_eq!(parse_flags("B", &names, |a, b| a | b, 0), 2);
        assert_eq!(parse_flags("", &names, |a, b| a | b, 0), 0);
        assert_eq!(format_flags(3u32, &names, |v, f| v & f == f, 0, "None"), "A, B");
        assert_eq!(format_flags(2u32, &names, |v, f| v & f == f, 0, "None"), "B");
        assert_eq!(format_flags(0u32, &names, |v, f| v & f == f, 0, "None"), "None");
    }
    #[test]
    fn half_subnormals_and_ties() {
        let tiny = 2f32.powi(-24);
        assert_eq!(f32_to_f16(tiny), 1);
        assert_eq!(f16_to_f32(1), tiny);
        assert_eq!(f16_to_f32(f32_to_f16(3.0 * tiny)), 3.0 * tiny);
        // exactly halfway between 0x3C00 and 0x3C01: ties to the even one
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3C00);
        // exactly halfway between 0x3C01 and 0x3C02: ties to the even one
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3C02);
    }
    #[test]
    fn hashes_and_halves() {
        assert_eq!(parse_hash("hash_DEADBEEF"), 0xDEADBEEF);
        assert_eq!(parse_hash("prop_a"), crate::rage_joaat("prop_a"));
        assert_eq!(parse_hash(""), 0);
        for f in [0.0f32, 1.0, -2.5, 0.333_251_95, 65504.0] { assert_eq!(f16_to_f32(f32_to_f16(f)), f); }
        assert_eq!(f32_to_f16(1.0), 0x3C00);
    }
}
