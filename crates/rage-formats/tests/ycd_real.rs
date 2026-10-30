//! Checks the clip dictionary reader and writer against real `.ycd` files and the XML another tool
//! made from them. Point `YCD_CHECK_DIR` at a folder holding `x.ycd` next to `x.ycd.xml`; the
//! test does nothing when it is not set.

use std::path::PathBuf;

use rage_formats::ycd::{from_xml, parse_ycd, serialize_ycd, to_xml};
use rage_formats::ycd_xml::parse_hash;
use roxmltree::{Document, Node};

fn dir() -> Option<PathBuf> {
    std::env::var_os("YCD_CHECK_DIR").map(PathBuf::from)
}

/// An element as plain data both writers' output reduces to: hash text becomes the hash it names,
/// numbers become numbers.
#[derive(Debug, PartialEq)]
enum Canon {
    Num(f64),
    Text(String),
    Node(String, Vec<(String, Canon)>, Vec<Canon>),
}

fn canon_text(t: &str, hashy: bool) -> Canon {
    let t = t.trim();
    if hashy {
        if let Ok(h) = parse_hash(t) {
            return Canon::Text(format!("{h:08X}"));
        }
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.len() == 1 {
        if let Ok(v) = words[0].parse::<f64>() {
            return Canon::Num(v);
        }
    }
    Canon::Text(words.join(" "))
}

fn canon(n: Node) -> Canon {
    let name = n.tag_name().name().to_string();
    let hashy_tag = matches!(name.as_str(), "Hash" | "NameHash" | "UnkHash" | "Unknown1C" | "AnimationHash")
        || (name == "Value" && n.parent().and_then(|p| p.children().find(|c| c.tag_name().name() == "Type")).and_then(|t| t.attribute("value")) == Some("HashString"));
    let mut attrs: Vec<(String, Canon)> = n.attributes().map(|a| (a.name().to_string(), canon_text(a.value(), false))).collect();
    attrs.sort_by(|a, b| a.0.cmp(&b.0));
    let kids: Vec<Node> = n.children().filter(|c| c.is_element()).collect();
    if kids.is_empty() {
        let text = n.text().unwrap_or("");
        // a list of numbers in one element compares as numbers, whatever the line breaks
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() > 1 && words.iter().all(|w| w.parse::<f64>().is_ok()) {
            return Canon::Node(name, attrs, words.iter().map(|w| Canon::Num(w.parse().unwrap())).collect());
        }
        return Canon::Node(name, attrs, if text.trim().is_empty() { vec![] } else { vec![canon_text(text, hashy_tag)] });
    }
    Canon::Node(name, attrs, kids.into_iter().map(canon).collect())
}

/// The first place two canonical trees differ, as a path.
fn first_diff(a: &Canon, b: &Canon, path: &str) -> Option<String> {
    match (a, b) {
        (Canon::Num(x), Canon::Num(y)) => {
            let tol = 1e-5 * x.abs().max(y.abs()).max(1e-3);
            ((x - y).abs() > tol).then(|| format!("{path}: {x} vs {y}"))
        }
        (Canon::Text(x), Canon::Text(y)) => (x != y).then(|| format!("{path}: '{x}' vs '{y}'")),
        (Canon::Node(n1, a1, k1), Canon::Node(n2, a2, k2)) => {
            if n1 != n2 {
                return Some(format!("{path}: <{n1}> vs <{n2}>"));
            }
            let here = format!("{path}/{n1}");
            if a1.len() != a2.len() {
                return Some(format!("{here}: attributes {:?} vs {:?}", a1.iter().map(|x| &x.0).collect::<Vec<_>>(), a2.iter().map(|x| &x.0).collect::<Vec<_>>()));
            }
            for ((na, va), (nb, vb)) in a1.iter().zip(a2) {
                if na != nb {
                    return Some(format!("{here}: attribute {na} vs {nb}"));
                }
                if let Some(d) = first_diff(va, vb, &format!("{here}@{na}")) {
                    return Some(d);
                }
            }
            if k1.len() != k2.len() {
                return Some(format!("{here}: {} children vs {}", k1.len(), k2.len()));
            }
            for (i, (x, y)) in k1.iter().zip(k2).enumerate() {
                if let Some(d) = first_diff(x, y, &format!("{here}[{i}]")) {
                    return Some(d);
                }
            }
            None
        }
        _ => Some(format!("{path}: different kinds")),
    }
}

fn canon_of(xml: &str) -> Canon {
    let doc = Document::parse(xml).expect("xml");
    canon(doc.root_element())
}

#[test]
fn reader_matches_the_reference_xml() {
    let Some(dir) = dir() else { return };
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ycd") {
            continue;
        }
        let reference = path.with_extension("ycd.xml");
        if !reference.exists() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let parsed = parse_ycd(&std::fs::read(&path).unwrap()).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let mine = canon_of(&to_xml(&parsed));
        let theirs = canon_of(&std::fs::read_to_string(&reference).unwrap());
        if let Some(d) = first_diff(&mine, &theirs, "") {
            panic!("{name}: my XML differs from the reference at {d}");
        }
        checked += 1;
    }
    assert!(checked > 0, "no .ycd/.ycd.xml pairs in {dir:?}");
    eprintln!("reader matched the reference XML on {checked} files");
}

#[test]
fn writer_round_trips_real_files() {
    let Some(dir) = dir() else { return };
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ycd") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let original = parse_ycd(&std::fs::read(&path).unwrap()).unwrap();
        // binary -> XML -> model -> binary -> model
        let from_text = from_xml(&to_xml(&original)).unwrap_or_else(|e| panic!("{name}: reading my own XML: {e:#}"));
        let bytes = serialize_ycd(&from_text).unwrap_or_else(|e| panic!("{name}: writing: {e:#}"));
        let back = parse_ycd(&bytes).unwrap_or_else(|e| panic!("{name}: reading what I wrote: {e:#}"));
        // channels and hash-table order survive; compare through the XML, which is canonical
        let a = canon_of(&to_xml(&original));
        let b = canon_of(&to_xml(&back));
        if let Some(d) = first_diff(&a, &b, "") {
            panic!("{name}: written file differs from the original at {d}");
        }
        std::fs::write(std::env::temp_dir().join(format!("rewritten_{name}")), &bytes).unwrap();
        checked += 1;
    }
    assert!(checked > 0, "no .ycd files in {dir:?}");
    eprintln!("writer round-tripped {checked} real files");
}
