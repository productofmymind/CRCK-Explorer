//! Does a Meta or PSO file survive being read and written again? The archetype editor edits a tree of values and saves it with the
//! writer from `rage-formats`; this module measures what that costs, so a file the writer would damage is known before it is saved.
//!
//! The comparison is on the *trees*, not the bytes: the resource is recompressed and padded differently, so byte-identical output is
//! not expected. A difference is a value that changed or a member that was dropped. A member the writer adds with its default value
//! (an older, shorter structure layout being upgraded to the current one) is counted as an upgrade, not a loss.
use anyhow::{Context, Result};
use rage_formats::{build_meta, build_pso, dump_meta, dump_pso, is_pso, MetaValue, NameTable, Schema};

/// How the file is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// RSC7 resource holding Meta blocks (the usual `.ytyp` / `.ymap`)
    Meta,
    /// PSO (big-endian sections; some DLC `.ytyp`)
    Pso,
}

/// One value that did not come back as it went in.
#[derive(Debug, Clone, PartialEq)]
pub struct Difference {
    /// e.g. `.archetypes[3].extensions[0].instances[2].posn[1]`
    pub path: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone)]
pub struct Fidelity {
    pub container: Container,
    pub archetypes: usize,
    pub mlos: usize,
    /// the first differences (at most [`MAX_LISTED`]); `total_differences` counts all of them
    pub differences: Vec<Difference>,
    pub total_differences: usize,
    /// members the writer added with their default value (an old layout upgraded)
    pub upgraded_fields: usize,
    /// what the reader could not decode
    pub read_warnings: Vec<String>,
    /// what the writer had to leave out
    pub write_warnings: Vec<String>,
    /// the rebuilt file
    pub rebuilt: Vec<u8>,
}

pub const MAX_LISTED: usize = 20;

impl Fidelity {
    /// True when every value came back (an upgraded layout still counts as lossless).
    pub fn is_lossless(&self) -> bool {
        self.total_differences == 0 && self.read_warnings.is_empty() && self.write_warnings.is_empty()
    }
}

/// The schema to write with: the built-in table, with the structure definitions the file itself carries on top (they win).
fn schema_for(data: &[u8]) -> Schema {
    let mut s = Schema::builtin().clone();
    if let Ok(own) = Schema::from_file(data) {
        s.merge(own);
    }
    s
}

/// Read `data`, write it again, read that, and compare the two trees.
pub fn check(data: &[u8]) -> Result<Fidelity> {
    let container = if is_pso(data) { Container::Pso } else { Container::Meta };
    let dump = match container {
        Container::Pso => dump_pso(data),
        Container::Meta => dump_meta(data),
    }
    .context("reading the file")?;
    let schema = schema_for(data);
    let written = match container {
        Container::Pso => build_pso(&dump.root, &schema),
        Container::Meta => build_meta(&dump.root, &schema),
    }
    .context("writing the file again")?;
    let again = match container {
        Container::Pso => dump_pso(&written.bytes),
        Container::Meta => dump_meta(&written.bytes),
    }
    .context("reading the rewritten file")?;

    let names = NameTable::core();
    let mut c = Cmp { names: &names, out: vec![], total: 0, upgraded: 0 };
    c.value(&dump.root, &again.root, &mut String::new());

    let (archetypes, mlos) = count_archetypes(&dump.root);
    let mut read_warnings = dump.warnings;
    read_warnings.extend(again.warnings);
    Ok(Fidelity { container, archetypes, mlos, differences: c.out, total_differences: c.total, upgraded_fields: c.upgraded, read_warnings, write_warnings: written.warnings, rebuilt: written.bytes })
}

/// The text of an XML dump without the parts that differ only in how they are spelled (`itemType` attributes, blank lines, and any line
/// starting with one of `skip`). Two trees that print the same text hold the same values, whatever number types they use.
pub(crate) fn normalised_xml(xml: &str, skip: &[&str]) -> String {
    xml.lines()
        .map(|l| {
            let mut l = l.trim().to_string();
            while let Some(i) = l.find(" itemType=\"") {
                let end = l[i + 11..].find('"').map(|e| i + 11 + e + 1).unwrap_or(l.len());
                l.replace_range(i..end, "");
            }
            l
        })
        .filter(|l| !l.is_empty() && !skip.iter().any(|s| l.starts_with(s)))
        .collect::<Vec<_>>()
        .join("
")
}

fn count_archetypes(root: &MetaValue) -> (usize, usize) {
    let mlo = rage_formats::rage_joaat("CMloArchetypeDef");
    let Some(list) = root.as_struct().and_then(|s| s.field("archetypes")) else { return (0, 0) };
    let items = list.items();
    (items.len(), items.iter().filter(|i| i.as_struct().map(|s| s.type_hash == mlo).unwrap_or(false)).count())
}

struct Cmp<'a> {
    names: &'a NameTable,
    out: Vec<Difference>,
    total: usize,
    upgraded: usize,
}

impl Cmp<'_> {
    fn diff(&mut self, path: &str, before: String, after: String) {
        self.total += 1;
        if self.out.len() < MAX_LISTED {
            self.out.push(Difference { path: path.to_string(), before, after });
        }
    }

    fn value(&mut self, a: &MetaValue, b: &MetaValue, path: &mut String) {
        use MetaValue as V;
        match (a, b) {
            (V::Struct(x), V::Struct(y)) => {
                if x.type_hash != y.type_hash {
                    self.diff(path, self.names.resolve(x.type_hash).to_string(), self.names.resolve(y.type_hash).to_string());
                    return;
                }
                for (h, va) in &x.fields {
                    let keep = path.len();
                    path.push('.');
                    path.push_str(&self.names.resolve(*h));
                    match y.get(*h) {
                        Some(vb) => self.value(va, vb, path),
                        None => self.diff(path, short(va), "(dropped)".into()),
                    }
                    path.truncate(keep);
                }
                for (h, vb) in &y.fields {
                    if x.get(*h).is_none() {
                        if is_default(vb) {
                            self.upgraded += 1;
                        } else {
                            let keep = path.len();
                            path.push('.');
                            path.push_str(&self.names.resolve(*h));
                            self.diff(path, "(absent)".into(), short(vb));
                            path.truncate(keep);
                        }
                    }
                }
            }
            (V::Array(x), V::Array(y)) => {
                // the element type name of an empty array is cosmetic, so only the items are compared
                if x.items.len() != y.items.len() {
                    self.diff(path, format!("{} items", x.items.len()), format!("{} items", y.items.len()));
                    return;
                }
                for (i, (va, vb)) in x.items.iter().zip(&y.items).enumerate() {
                    let keep = path.len();
                    path.push_str(&format!("[{i}]"));
                    self.value(va, vb, path);
                    path.truncate(keep);
                }
            }
            (V::Map(x), V::Map(y)) => {
                if x.len() != y.len() {
                    self.diff(path, format!("{} entries", x.len()), format!("{} entries", y.len()));
                    return;
                }
                for (i, ((ka, va), (kb, vb))) in x.iter().zip(y).enumerate() {
                    let keep = path.len();
                    path.push_str(&format!("{{{i}}}"));
                    self.value(ka, kb, path);
                    self.value(va, vb, path);
                    path.truncate(keep);
                }
            }
            (V::F32(x), V::F32(y)) if x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()) => {}
            (V::Enum { value: x, .. }, V::Enum { value: y, .. }) if x == y => {}
            (V::Flags { bits: x, .. }, V::Flags { bits: y, .. }) if x == y => {}
            (x, y) if x == y => {}
            (x, y) if loose_eq(x, y) => {}
            (x, y) => self.diff(path, short(x), short(y)),
        }
    }
}

fn number_of(v: &MetaValue) -> Option<f64> {
    use MetaValue as V;
    Some(match v {
        V::Bool(b) => *b as u8 as f64,
        V::I8(x) => *x as f64,
        V::U8(x) => *x as f64,
        V::I16(x) => *x as f64,
        V::U16(x) => *x as f64,
        V::I32(x) => *x as f64,
        V::U32(x) => *x as f64,
        V::I64(x) => *x as f64,
        V::U64(x) => *x as f64,
        V::F32(x) => *x as f64,
        V::Enum { value, .. } => *value as f64,
        V::Flags { bits, .. } => *bits as f64,
        _ => return None,
    })
}

/// The same value in another spelling: numbers of different types, a hash and the name it stands for, an enum and its member's name.
/// A tree read from XML (loosely typed) is equal to the same tree read back from bytes (exactly typed) by this.
fn loose_eq(a: &MetaValue, b: &MetaValue) -> bool {
    use MetaValue as V;
    if let (Some(x), Some(y)) = (number_of(a), number_of(b)) {
        return x == y;
    }
    let hash_of = |s: &str| {
        let t = s.trim();
        if let Some(h) = t.strip_prefix("hash_").or_else(|| t.strip_prefix("0x")) {
            if let Ok(v) = u32::from_str_radix(h, 16) {
                return vec![v];
            }
        }
        vec![rage_formats::rage_joaat(t), rage_formats::rage_joaat(&t.to_lowercase())]
    };
    match (a, b) {
        (V::Hash(h), V::Str(s)) | (V::Str(s), V::Hash(h)) => s.trim().is_empty() && *h == 0 || hash_of(s).contains(h),
        (V::Hash(h), x) | (x, V::Hash(h)) => number_of(x).map(|n| n == *h as f64).unwrap_or(false),
        (V::Enum { value, name, .. }, V::Str(s)) | (V::Str(s), V::Enum { value, name, .. }) => name.map(|n| hash_of(s).contains(&n)).unwrap_or(false) || s.trim().parse::<i32>().ok() == Some(*value) || (s.trim().is_empty() && *value == 0),
        (V::Flags { bits, .. }, V::Str(s)) | (V::Str(s), V::Flags { bits, .. }) => s.trim().is_empty() && *bits == 0,
        (V::Vec2(_) | V::Vec3(_) | V::Vec4(_), _) | (_, V::Vec2(_) | V::Vec3(_) | V::Vec4(_)) => false,
        _ => false,
    }
}

/// Compare two trees the way [`check`] does (a member the second has and the first lacks, with a default value, is not a difference).
/// Returns the first differences and how many there are.
pub fn compare_trees(a: &MetaValue, b: &MetaValue, names: &NameTable) -> (Vec<Difference>, usize) {
    let mut c = Cmp { names, out: vec![], total: 0, upgraded: 0 };
    c.value(a, b, &mut String::new());
    (c.out, c.total)
}

/// What a writer adds when it upgrades an older layout: a zero, an empty string or array, a null.
fn is_default(v: &MetaValue) -> bool {
    use MetaValue as V;
    match v {
        V::Null => true,
        V::Bool(b) => !*b,
        V::I8(x) => *x == 0,
        V::U8(x) => *x == 0,
        V::I16(x) => *x == 0,
        V::U16(x) => *x == 0,
        V::I32(x) => *x == 0,
        V::U32(x) => *x == 0,
        V::I64(x) => *x == 0,
        V::U64(x) => *x == 0,
        V::F32(x) => *x == 0.0,
        V::Hash(x) => *x == 0,
        V::Str(s) => s.is_empty(),
        V::Enum { value, .. } => *value == 0,
        V::Flags { bits, .. } => *bits == 0,
        V::Bytes(b) => b.iter().all(|x| *x == 0),
        V::Array(a) => a.items.is_empty(),
        V::Map(m) => m.is_empty(),
        V::Struct(s) => s.fields.iter().all(|(_, f)| is_default(f)),
        V::Vec2(a) => a.x == 0.0 && a.y == 0.0,
        V::Vec3(a) => a.x == 0.0 && a.y == 0.0 && a.z == 0.0,
        V::Vec4(a) => a.x == 0.0 && a.y == 0.0 && a.z == 0.0 && a.w == 0.0,
    }
}

fn short(v: &MetaValue) -> String {
    let s = format!("{v:?}");
    if s.len() > 90 {
        format!("{}...", s.chars().take(90).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A light effect with a float array (`CLightAttrDef.posn`), as an interior `.ytyp` has, must keep its floats through the writer.
    /// The vendored writer used to send them through the integer path (zeros, denormals, NaN).
    #[test]
    fn a_float_array_survives_the_writer() {
        let xml = r#"<CMapTypes>
  <extensions/>
  <archetypes>
    <Item type="CBaseArchetypeDef">
      <lodDist value="100"/>
      <name>test_light_prop</name>
      <extensions>
        <Item type="CExtensionDefLightEffect">
          <name>light</name>
          <offsetPosition x="0" y="0" z="0"/>
          <instances itemType="CLightAttrDef">
            <Item>
              <posn itemType="float">0.0068 -0.18 0.0106</posn>
              <dir itemType="float">0.5 -0.25 2</dir>
            </Item>
          </instances>
        </Item>
      </extensions>
    </Item>
  </archetypes>
  <name>test_types</name>
</CMapTypes>"#;
        let tree = rage_formats::from_xml(xml).expect("the XML reads");
        let schema = Schema::builtin();
        let bytes = build_meta(&tree, schema).expect("the tree builds").bytes;
        let report = check(&bytes).expect("the built file checks");
        assert!(report.is_lossless(), "differences: {:?}, warnings: {:?}", report.differences, report.write_warnings);
        // and the values are the ones we put in, not just equal to themselves
        let back = dump_meta(&bytes).unwrap().root;
        let arch = back.as_struct().unwrap().field("archetypes").unwrap().items()[0].as_struct().unwrap().clone();
        let ext = arch.field("extensions").unwrap().items()[0].as_struct().unwrap().clone();
        let light = ext.field("instances").unwrap().items()[0].as_struct().unwrap().clone();
        let posn: Vec<f32> = light.field("posn").unwrap().items().iter().map(|v| v.as_f32().unwrap()).collect();
        assert_eq!(posn, vec![0.0068, -0.18, 0.0106]);
    }
}
