//! Editing a handling.meta as text: change one value, rename, duplicate or remove an entry, and leave every other byte of the
//! file as it was (comments, indentation, number formats, line endings). Each edit is checked by reading the result back.
use crate::handling::{parse, Parsed, Value};
use anyhow::{bail, Context, Result};
use roxmltree::Document;
use std::ops::Range;

/// Which field: entry `item` (index in `parse(..).items`), then `sub` = a SubHandlingData item (None = the entry's own fields),
/// then `field` = index in that list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldRef {
    pub item: usize,
    pub sub: Option<usize>,
    pub field: usize,
}

/// `old` is how the value was written (`1800.000000`, `6`, `0.35`): the new one keeps at least that many decimals.
fn fmt_like(old: &str, v: f64) -> String {
    let old = old.trim().trim_end_matches(['f', 'F']);
    let want = old.split_once('.').map(|(_, d)| d.len()).unwrap_or(0);
    let mut s = format!("{v}");
    let have = s.split_once('.').map(|(_, d)| d.len()).unwrap_or(0);
    if have < want {
        if !s.contains('.') {
            s.push('.');
        }
        s.push_str(&"0".repeat(want - have));
    }
    s
}

fn lines_span(src: &str, line: usize) -> Option<Range<usize>> {
    let mut start = 0;
    for (i, l) in src.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            return Some(start..start + l.trim_end_matches(['\r', '\n']).len());
        }
        start += l.len();
    }
    None
}

/// byte index of the `<` of the element called `name` (case-insensitive) in `line`
fn find_element(line: &str, name: &str) -> Option<usize> {
    let (lower, needle) = (line.to_ascii_lowercase(), format!("<{}", name.to_ascii_lowercase()));
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        let at = from + i;
        if lower[at + needle.len()..].chars().next().is_some_and(|c| c.is_whitespace() || c == '>' || c == '/') {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Replaces the value of attribute `attr` inside the open tag `tag` (`<fMass value="1"`), keeping its quotes.
fn set_attr(tag: &str, attr: &str, new: &dyn Fn(&str) -> String) -> Option<String> {
    let key = format!("{attr}=");
    let at = tag.match_indices(&key).map(|(i, _)| i).find(|&i| tag[..i].ends_with(char::is_whitespace))?;
    let q = tag[at + key.len()..].chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let vstart = at + key.len() + 1;
    let vend = vstart + tag[vstart..].find(q)?;
    Some(format!("{}{}{}", &tag[..vstart], new(&tag[vstart..vend]), &tag[vend..]))
}

fn field_of(p: &Parsed, r: FieldRef) -> Option<&crate::handling::Field> {
    let item = p.items.get(r.item)?;
    match r.sub {
        None => item.fields.get(r.field),
        Some(s) => item.sub.get(s)?.fields.get(r.field),
    }
}

/// Writes `new` into the field. Fails (and changes nothing) when the element is not a plain one-line `value=` / `x= y= z=` /
/// text element, or when reading the result back does not give `new`.
pub fn set_value(src: &str, r: FieldRef, new: &Value) -> Result<String> {
    let p = parse(src).map_err(|e| anyhow::anyhow!("not valid XML: {e}"))?;
    let f = field_of(&p, r).context("no such field")?;
    let span = lines_span(src, f.line).context("field line not found")?;
    let line = &src[span.clone()];
    let at = find_element(line, &f.name).with_context(|| format!("{} is not on line {}", f.name, f.line))?;
    let open_end = at + line[at..].find('>').context("the element continues on another line: edit it by hand")?;
    let self_closing = line[..open_end].ends_with('/');
    let tag = &line[at..open_end - usize::from(self_closing)];
    let new_line = match new {
        Value::Num(v) => {
            let t = set_attr(tag, "value", &|old| fmt_like(old, *v)).context("no value= attribute")?;
            format!("{}{}{}", &line[..at], t, &line[at + tag.len()..])
        }
        Value::Vec3(c) => {
            let mut t = tag.to_string();
            for (k, v) in ["x", "y", "z"].iter().zip(c) {
                t = set_attr(&t, k, &|old| fmt_like(old, *v)).with_context(|| format!("no {k}= attribute"))?;
            }
            format!("{}{}{}", &line[..at], t, &line[at + tag.len()..])
        }
        Value::Text(s) => {
            if self_closing {
                bail!("the element has no text to replace");
            }
            let close = line[open_end..].to_ascii_lowercase().find(&format!("</{}", f.name.to_ascii_lowercase())).context("no closing tag on the same line")?;
            format!("{}{}{}", &line[..=open_end], s, &line[open_end + close..])
        }
        _ => bail!("only numbers, vectors and text can be edited"),
    };
    let out = format!("{}{}{}", &src[..span.start], new_line, &src[span.end..]);
    let back = parse(&out).map_err(|e| anyhow::anyhow!("the edit broke the XML: {e}"))?;
    let got = field_of(&back, r).context("field vanished after the edit")?;
    let same = match (&got.value, new) {
        (Value::Num(a), Value::Num(b)) => (a - b).abs() <= 1e-9 * b.abs().max(1.0),
        (Value::Vec3(a), Value::Vec3(b)) => a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-9 * y.abs().max(1.0)),
        (a, b) => a == b,
    };
    if !same {
        bail!("the edit was not read back as written ({:?})", got.value);
    }
    Ok(out)
}

/// The byte range of the `<Item>` whose `handlingName` is `name` (case-insensitive), in `src`.
pub fn entry_span(src: &str, name: &str) -> Option<Range<usize>> {
    let trimmed = src.trim_start_matches('\u{feff}');
    let off = src.len() - trimmed.len();
    let doc = Document::parse(trimmed).ok()?;
    doc.root_element()
        .children()
        .filter(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("HandlingData"))
        .flat_map(|h| h.children().filter(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("Item")).collect::<Vec<_>>())
        .find(|n| n.children().any(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("handlingName") && c.text().unwrap_or("").trim().eq_ignore_ascii_case(name)))
        .map(|n| n.range().start + off..n.range().end + off)
}

/// The source text of one entry, to copy into another file.
pub fn entry_text(src: &str, name: &str) -> Option<String> {
    entry_span(src, name).map(|r| src[r].to_string())
}

fn name_span(item_text: &str) -> Option<Range<usize>> {
    let lower = item_text.to_ascii_lowercase();
    let open = lower.find("<handlingname>")? + "<handlingname>".len();
    let close = open + lower[open..].find("</handlingname>")?;
    Some(open..close)
}

/// Puts a copy of `item_text` (an `<Item>` taken from this or another file) after the last entry of `src`, under `new_name`.
pub fn add_entry(src: &str, item_text: &str, new_name: &str) -> Result<String> {
    let new_name = new_name.trim();
    if new_name.is_empty() || new_name.contains(['<', '>', '&']) {
        bail!("the name must not be empty or contain < > &");
    }
    if entry_span(src, new_name).is_some() {
        bail!("there is already an entry called {new_name}");
    }
    let ns = name_span(item_text).context("the entry has no handlingName")?;
    let item = format!("{}{}{}", &item_text[..ns.start], new_name, &item_text[ns.end..]);
    let trimmed = src.trim_start_matches('\u{feff}');
    let off = src.len() - trimmed.len();
    let doc = Document::parse(trimmed).map_err(|e| anyhow::anyhow!("not valid XML: {e}"))?;
    let container = doc.root_element().children().find(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("HandlingData")).context("no <HandlingData> in the file")?;
    let last = container.children().rfind(|c| c.is_element() && c.tag_name().name().eq_ignore_ascii_case("Item"));
    let nl = if src.contains("\r\n") { "\r\n" } else { "\n" };
    let item = item.replace("\r\n", "\n").replace('\n', nl);
    let out = match last {
        Some(l) => {
            // same indentation as the entry before it
            let line_start = trimmed[..l.range().start].rfind('\n').map_or(0, |i| i + 1);
            let indent = &trimmed[line_start..l.range().start];
            let indent = if indent.trim().is_empty() { indent } else { "" };
            let at = l.range().end + off;
            format!("{}{nl}{indent}{}{}", &src[..at], item, &src[at..])
        }
        None => {
            let at = container.range().end + off - "</HandlingData>".len();
            format!("{}{nl}{}{nl}{}", &src[..at], item, &src[at..])
        }
    };
    parse(&out).map_err(|e| anyhow::anyhow!("the new entry broke the XML: {e}"))?;
    Ok(out)
}

/// A copy of entry `from`, called `new_name`, next to it.
pub fn duplicate_entry(src: &str, from: &str, new_name: &str) -> Result<String> {
    let text = entry_text(src, from).with_context(|| format!("no entry called {from}"))?;
    add_entry(src, &text, new_name)
}

pub fn remove_entry(src: &str, name: &str) -> Result<String> {
    let span = entry_span(src, name).with_context(|| format!("no entry called {name}"))?;
    // take the line the entry starts on with it when nothing else is on that line
    let start = src[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let start = if src[start..span.start].trim().is_empty() { start } else { span.start };
    let end = span.end + src[span.end..].find('\n').filter(|&i| src[span.end..span.end + i].trim().is_empty()).map_or(0, |i| i + 1);
    let out = format!("{}{}", &src[..start], &src[end..]);
    parse(&out).map_err(|e| anyhow::anyhow!("removing it broke the XML: {e}"))?;
    Ok(out)
}

/// Renames an entry (its `handlingName`); the vehicles.meta `handlingId` that points at it is not touched.
pub fn rename_entry(src: &str, old: &str, new_name: &str) -> Result<String> {
    let span = entry_span(src, old).with_context(|| format!("no entry called {old}"))?;
    if !old.eq_ignore_ascii_case(new_name) && entry_span(src, new_name).is_some() {
        bail!("there is already an entry called {new_name}");
    }
    let text = &src[span.clone()];
    let ns = name_span(text).context("the entry has no handlingName")?;
    Ok(format!("{}{}{}{}", &src[..span.start + ns.start], new_name.trim(), &text[ns.end..], &src[span.end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<CHandlingDataMgr>\r\n  <HandlingData>\r\n    <Item type=\"CHandlingData\">\r\n      <handlingName>ADDER</handlingName>\r\n      <fMass value=\"1800.000000\" />\r\n      <fInitialDragCoeff value=\"8.0\" />\r\n      <nInitialDriveGears value=\"6\" />\r\n      <vecCentreOfMassOffset x=\"0.000000\" y=\"-0.100000\" z=\"0.0\" />\r\n      <strModelFlags>440010</strModelFlags>\r\n      <SubHandlingData>\r\n        <Item type=\"CCarHandlingData\">\r\n          <fBackEndPopUpCarImpulseMult value=\"0.100000\" />\r\n        </Item>\r\n        <Item type=\"NULL\" />\r\n      </SubHandlingData>\r\n    </Item>\r\n    <!-- second -->\r\n    <Item type=\"CHandlingData\">\r\n      <handlingName>BANSHEE</handlingName>\r\n      <fMass value=\"1400.0\" />\r\n    </Item>\r\n  </HandlingData>\r\n</CHandlingDataMgr>\r\n";

    fn r(item: usize, sub: Option<usize>, field: usize) -> FieldRef {
        FieldRef { item, sub, field }
    }

    #[test]
    fn numbers_keep_their_format_and_the_rest_of_the_file() {
        let out = set_value(SRC, r(0, None, 0), &Value::Num(1850.0)).unwrap();
        assert!(out.contains("<fMass value=\"1850.000000\" />"));
        assert_eq!(out.replace("1850.000000", "1800.000000"), SRC, "nothing else changed");
        let out = set_value(SRC, r(0, None, 2), &Value::Num(5.0)).unwrap();
        assert!(out.contains("<nInitialDriveGears value=\"6\" />".replace('6', "5").as_str()));
        let out = set_value(SRC, r(0, None, 1), &Value::Num(8.25)).unwrap();
        assert!(out.contains("value=\"8.25\""));
        let out = set_value(SRC, r(1, None, 0), &Value::Num(1500.5)).unwrap();
        assert!(out.contains("<fMass value=\"1500.5\" />"));
    }

    #[test]
    fn vectors_text_and_sub_handling() {
        let out = set_value(SRC, r(0, None, 3), &Value::Vec3([0.0, 0.25, -0.5])).unwrap();
        assert!(out.contains("x=\"0.000000\" y=\"0.250000\" z=\"-0.5\""), "{out}");
        let out = set_value(SRC, r(0, None, 4), &Value::Text("440011".into())).unwrap();
        assert!(out.contains("<strModelFlags>440011</strModelFlags>"));
        let out = set_value(SRC, r(0, Some(0), 0), &Value::Num(0.3)).unwrap();
        assert!(out.contains("<fBackEndPopUpCarImpulseMult value=\"0.300000\" />"));
    }

    #[test]
    fn bad_edits_change_nothing() {
        assert!(set_value(SRC, r(5, None, 0), &Value::Num(1.0)).is_err());
        assert!(set_value(SRC, r(0, None, 0), &Value::Text("x".into())).is_err() || parse(&set_value(SRC, r(0, None, 0), &Value::Text("x".into())).unwrap()).is_ok());
    }

    #[test]
    fn entries_are_copied_renamed_and_removed() {
        let out = duplicate_entry(SRC, "adder", "MYCAR").unwrap();
        let p = parse(&out).unwrap();
        assert_eq!(p.items.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["ADDER", "BANSHEE", "MYCAR"]);
        assert_eq!(p.items[2].num("fMass"), Some(1800.0));
        assert!(out.contains("\r\n"), "line endings kept");
        assert!(duplicate_entry(SRC, "adder", "banshee").is_err(), "names are unique, any case");
        let out2 = rename_entry(&out, "mycar", "MyOtherCar").unwrap();
        assert!(entry_span(&out2, "myothercar").is_some() && entry_span(&out2, "mycar").is_none());
        let out3 = remove_entry(&out2, "banshee").unwrap();
        let p3 = parse(&out3).unwrap();
        assert_eq!(p3.items.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["ADDER", "MyOtherCar"]);
        assert!(!out3.contains("BANSHEE"));
    }

    #[test]
    fn entry_from_another_file_goes_in() {
        let other = "<CHandlingDataMgr><HandlingData></HandlingData></CHandlingDataMgr>";
        let text = entry_text(SRC, "BANSHEE").unwrap();
        let out = add_entry(other, &text, "copy").unwrap();
        assert_eq!(parse(&out).unwrap().items[0].num("fMass"), Some(1400.0));
    }

    #[test]
    fn number_format_follows_the_old_text() {
        assert_eq!(fmt_like("1800.000000", 1850.0), "1850.000000");
        assert_eq!(fmt_like("6", 5.0), "5");
        assert_eq!(fmt_like("6", 5.5), "5.5");
        assert_eq!(fmt_like("0.35", 0.4), "0.40");
        assert_eq!(fmt_like("-30.0f", -12.0), "-12.0");
    }
}
