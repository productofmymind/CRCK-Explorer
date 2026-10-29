//! Models to and from the reference tool's XML: `.ydr`, `.ybn` and `.yft` both ways. A `.yft`'s XML holds its drawables
//! as editable XML and the rest of the file (physics, bounds, names, cloth) as bytes with links, so only a `<Fragment>`
//! this tool exported can be built back (see `rage_formats::blocks::yft`).
use anyhow::{bail, Result};
use rage_formats::NameTable;
use std::path::Path;

/// Whether `name` is a file this can write as XML.
pub fn can_export(name: &str) -> bool {
    matches!(ext(name).as_str(), "ydr" | "ybn" | "yft")
}

fn ext(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The XML of a `.ydr`, `.ybn` or `.yft` (`data` is the file; `name` picks the format).
pub fn export(name: &str, data: &[u8]) -> Result<String> {
    let names = NameTable::core();
    match ext(name).as_str() {
        "ydr" => rage_formats::dump_ydr_xml(data, &names, None),
        "ybn" => rage_formats::dump_ybn_xml(data),
        "yft" => rage_formats::dump_yft_xml(data, &names, None),
        e => bail!("no XML export for .{e} files"),
    }
}

/// The binary extension a model XML converts to, from its root element; `None` for any other XML (a .meta, say), which
/// stays a plain file.
pub fn import_ext(xml: &str) -> Option<&'static str> {
    let mut rest = xml.trim_start_matches('\u{feff}');
    loop {
        let at = rest.find('<')?;
        rest = &rest[at + 1..];
        if let Some(r) = rest.strip_prefix("!--") {
            rest = &r[r.find("-->")? + 3..];
        } else if rest.starts_with('?') || rest.starts_with('!') {
            rest = &rest[rest.find('>')?..];
        } else {
            let tag: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
            return match tag.as_str() {
                "Drawable" => Some("ydr"),
                "BoundsFile" | "Bounds" => Some("ybn"),
                "Fragment" => Some("yft"),
                _ => None,
            };
        }
    }
}

/// The binary for a model XML, and a line for each thing it asks for that the game may not accept. `dds_dir` is where the
/// XML's textures are (the folder of the XML file).
pub fn import(xml: &str, dds_dir: Option<&Path>) -> Result<(Vec<u8>, Vec<String>)> {
    match import_ext(xml) {
        Some("ydr") => rage_formats::build_ydr_from_xml_checked(xml, dds_dir).map(|b| (b.bytes, b.warnings)),
        Some("ybn") => rage_formats::build_ybn_from_xml_checked(xml).map(|b| (b.bytes, b.warnings)),
        Some(_) => rage_formats::build_yft_from_xml_checked(xml, dds_dir).map(|b| (b.bytes, b.warnings)),
        None => bail!("not a model XML (its root element is not <Drawable>, <BoundsFile> or <Fragment>)"),
    }
}

/// A one-drawable `<Fragment>` document for tests (a zeroed header, so not a fragment the game would load).
#[doc(hidden)]
pub fn test_fragment_xml() -> String {
    let ydr = rage_formats::build_ydr_from_xml(include_str!("../../rage-formats/tests/fixtures/one_triangle.ydr.xml"), None).unwrap();
    let ydr_xml = rage_formats::dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap();
    rage_formats::blocks::yft_xml::test_fragment_xml(&ydr_xml, "tri")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_element_picks_the_format() {
        assert_eq!(import_ext("<?xml version=\"1.0\"?>\n<!-- c -->\n<Drawable>\n</Drawable>"), Some("ydr"));
        assert_eq!(import_ext("\u{feff}<BoundsFile/>"), Some("ybn"));
        assert_eq!(import_ext("<Fragment>"), Some("yft"));
        assert_eq!(import_ext("<CVehicleModelInfo__InitDataList/>"), None);
        assert_eq!(import_ext("no xml"), None);
    }


    #[test]
    fn a_fragment_xml_becomes_a_yft_and_exports_again() {
        let xml = test_fragment_xml();
        let (bytes, _warnings) = import(&xml, None).unwrap();
        assert_eq!(&bytes[..4], b"RSC7");
        let back = export("tri.yft", &bytes).unwrap();
        assert!(back.contains("<Name>tri</Name>") && back.contains("<Chunks>"), "{back}");
        let (again, _) = import(&back, None).unwrap();
        assert_eq!(export("tri.yft", &again).unwrap(), back);
        let err = import(&xml.replacen("<Name>tri</Name>", "<Name>x</Name>", 1), None).unwrap_err().to_string();
        assert!(err.contains("does not read back"), "{err}");
    }
}
