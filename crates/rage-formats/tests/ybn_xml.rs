use rage_formats::blocks::bounds::BoundBlock;
use rage_formats::blocks::ybn::{build_ybn_from_xml, dump_ybn_xml, read_ybn, write_ybn};

const XML: &str = include_str!("fixtures/composite.ybn.xml");

/// Compares line by line so a failure names the first line that differs.
fn assert_same_xml(got: &str, want: &str) {
    for (i, (a, b)) in got.lines().zip(want.lines()).enumerate() { assert_eq!(a, b, "line {}", i + 1); }
    assert_eq!(got.lines().count(), want.lines().count(), "line count");
}

#[test]
fn ybn_round_trips_through_xml_and_the_old_parser() {
    let ybn = build_ybn_from_xml(XML).unwrap();
    assert_same_xml(&dump_ybn_xml(&ybn).unwrap(), XML);
    assert_eq!(rage_formats::parse_ybn(&ybn).unwrap().triangles().len(), 2);
}

#[test]
fn a_ybn_read_back_writes_the_same_file() {
    let ybn = build_ybn_from_xml(XML).unwrap();
    let (mut g, root) = read_ybn(&ybn).unwrap();
    assert!(matches!(g.get::<BoundBlock>(root), BoundBlock::Composite(c) if c.children.len() == 2));
    assert!(write_ybn(&mut g, root).unwrap() == ybn, "the rewritten file differs");
}

#[test]
fn a_composite_inside_a_composite_is_refused_on_read() {
    let nested = XML.replacen("<Item type=\"Box\">", "<Item type=\"Composite\">", 1);
    let ybn = build_ybn_from_xml(&nested).unwrap();
    let err = read_ybn(&ybn).err().expect("nested composites are not read");
    assert!(err.to_string().contains("inside another composite"), "{err}");
}

#[test]
fn a_document_without_bounds_is_an_error() {
    assert!(build_ybn_from_xml("<BoundsFile />").is_err());
    assert!(build_ybn_from_xml("<BoundsFile><Bounds type=\"None\" /></BoundsFile>").is_err());
}

#[test]
fn the_checked_build_returns_the_file_and_its_xml() {
    let built = rage_formats::build_ybn_from_xml_checked(XML).unwrap();
    assert_eq!(built.bytes, build_ybn_from_xml(XML).unwrap());
    assert_same_xml(&built.xml, XML);
    assert!(built.warnings.is_empty());
}
