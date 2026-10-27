use rage_formats::blocks::drawable::{Drawable, DrawableModelsBlock};
use rage_formats::blocks::ydr::{build_ydr_from_xml, dump_ydr_xml, read_ydr};
use rage_formats::NameTable;

const XML: &str = include_str!("fixtures/one_triangle.ydr.xml");

#[test]
fn a_triangle_builds_and_the_old_parser_agrees() {
    let ydr = build_ydr_from_xml(XML, None).unwrap();
    let d = rage_formats::parse_ydr(&ydr).unwrap();
    assert_eq!(d.name, "tri_prop");
    let lod = d.best_lod().unwrap();
    assert_eq!(d.triangle_count(lod), 1);
    assert_eq!(d.geometry_count(), 1);
    assert_eq!(d.shader_count(), 1);
}

#[test]
fn dump_of_a_build_is_the_input() {
    let ydr = build_ydr_from_xml(XML, None).unwrap();
    let dumped = dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap();
    assert_eq!(dumped, XML);
}

#[test]
fn render_masks_and_lod_pointers_for_absent_lists() {
    let xml = XML.replace("DrawableModelsHigh", "DrawableModelsMedium");
    let ydr = build_ydr_from_xml(&xml, None).unwrap();
    let (g, root) = read_ydr(&ydr).unwrap();
    let d = g.get::<Drawable>(root);
    assert_eq!(d.render_mask_flags[0] >> 8 & 0xFF, 0, "no high models: high mask 0");
    assert_eq!(d.render_mask_flags[1] >> 8 & 0xFF, 255);
    let models = g.get::<DrawableModelsBlock>(d.models.unwrap());
    assert!(models.high.is_none() && models.med.as_ref().unwrap().len() == 1);
    assert_eq!(dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap(), xml);
}

/// Two geometries in a model (the outer box, the 16-byte padding of the shader mapping), bone
/// ids past four (the extra 8 bytes after the geometry) and a second LOD list.
#[test]
fn several_geometries_bone_ids_and_lods_round_trip() {
    let start = XML.find("        <Item>\n          <ShaderIndex").unwrap();
    let end = XML.find("      </Geometries>").unwrap();
    let geom = &XML[start..end];
    let second = geom
        .replace("<BoundingBoxMax x=\"1\" y=\"1\" z=\"1\" w=\"0\" />", "<BoundingBoxMax x=\"3\" y=\"1\" z=\"2\" w=\"0\" />\n          <BoneIDs>1, 2, 3, 4, 5</BoneIDs>")
        .replace("<ShaderIndex value=\"0\" />", "<ShaderIndex value=\"1\" />");
    let mut xml = XML.replace(geom, &format!("{geom}{second}"));
    let first_model = xml.find("    <Item>\n      <RenderMask").unwrap();
    let last_model = xml.find("  </DrawableModelsHigh>").unwrap();
    let models = xml[first_model..last_model].to_owned();
    xml = xml.replace("</DrawableModelsHigh>\n", &format!("</DrawableModelsHigh>\n  <DrawableModelsLow>\n{models}  </DrawableModelsLow>\n"));

    let ydr = build_ydr_from_xml(&xml, None).unwrap();
    let old = rage_formats::parse_ydr(&ydr).unwrap();
    assert_eq!(old.geometry_count(), 4);
    let (g, root) = read_ydr(&ydr).unwrap();
    let d = g.get::<Drawable>(root);
    let block = g.get::<DrawableModelsBlock>(d.models.unwrap());
    let model = g.get::<rage_formats::blocks::drawable::DrawableModel>(block.high.as_ref().unwrap()[0]);
    assert_eq!(model.shader_mapping(&g), vec![0, 1]);
    let bounds = model.bounds_data(&g);
    assert_eq!(bounds.len(), 3);
    assert_eq!(bounds[0].1.x, 3.0, "the outer box comes first");
    assert_eq!((d.render_mask_flags[2] >> 8) & 0xFF, 255);
    assert_eq!(dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap(), xml);
    // a graph read back writes the same file as the graph built from the XML
    let (mut g2, root2) = read_ydr(&ydr).unwrap();
    assert_eq!(rage_formats::blocks::ydr::write_ydr(&mut g2, root2).unwrap(), ydr);
}

#[test]
fn a_drawable_carries_its_bound() {
    let xml = XML.replace("</DrawableModelsHigh>\n", &format!("</DrawableModelsHigh>\n{}", include_str!("fixtures/box_bounds_fragment.xml")));
    assert_ne!(xml, XML);
    let ydr = build_ydr_from_xml(&xml, None).unwrap();
    assert_eq!(dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap(), xml);
    let (mut g, root) = read_ydr(&ydr).unwrap();
    let d = g.get::<Drawable>(root);
    let bound = d.bound.expect("the bound pointer is read");
    assert!(matches!(g.get::<rage_formats::blocks::bounds::BoundBlock>(bound), rage_formats::blocks::bounds::BoundBlock::Box(_)));
    assert!(g.get::<rage_formats::blocks::bounds::BoundBlock>(bound).common().pages.is_none(), "only the drawable root owns pages");
    assert_eq!(rage_formats::blocks::ydr::write_ydr(&mut g, root).unwrap(), ydr);
    assert_eq!(rage_formats::parse_ydr(&ydr).unwrap().triangle_count(rage_formats::parse_ydr(&ydr).unwrap().best_lod().unwrap()), 1);
}

#[test]
fn a_drawable_with_a_none_bound_has_none() {
    let xml = XML.replace("</DrawableModelsHigh>\n", "</DrawableModelsHigh>\n  <Bounds type=\"None\" />\n");
    let ydr = build_ydr_from_xml(&xml, None).unwrap();
    let (g, root) = read_ydr(&ydr).unwrap();
    assert!(g.get::<Drawable>(root).bound.is_none());
    assert_eq!(dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap(), XML);
}

#[test]
fn the_checked_build_returns_the_file_its_xml_and_the_warnings() {
    let built = rage_formats::build_ydr_from_xml_checked(XML, None).unwrap();
    assert_eq!(built.bytes, build_ydr_from_xml(XML, None).unwrap());
    assert_eq!(built.xml, XML);
    assert_eq!(built.warnings, ["shader 0 parameter DiffuseSampler: texture 'missing_tex' is not embedded (resolved at runtime from the archetype's txd)"]);
}

#[test]
fn a_bone_without_a_name_is_a_warning() {
    let skeleton = "  <Skeleton>\n    <Unknown1C value=\"0\" />\n    <Unknown50 value=\"0\" />\n    <Unknown54 value=\"0\" />\n    <Unknown58 value=\"0\" />\n    <Bones>\n      <Item>\n        <Name>root</Name>\n        <Tag value=\"0\" />\n        <Index value=\"0\" />\n        <ParentIndex value=\"-1\" />\n        <SiblingIndex value=\"-1\" />\n        <Flags>None</Flags>\n        <Translation x=\"0\" y=\"0\" z=\"0\" />\n        <Rotation x=\"0\" y=\"0\" z=\"0\" w=\"1\" />\n        <Scale x=\"1\" y=\"1\" z=\"1\" />\n        <TransformUnk x=\"0\" y=\"0\" z=\"0\" w=\"0\" />\n      </Item>\n      <Item>\n        <Name />\n        <Tag value=\"1\" />\n        <Index value=\"1\" />\n        <ParentIndex value=\"0\" />\n        <SiblingIndex value=\"-1\" />\n        <Flags>None</Flags>\n        <Translation x=\"0\" y=\"0\" z=\"0\" />\n        <Rotation x=\"0\" y=\"0\" z=\"0\" w=\"1\" />\n        <Scale x=\"1\" y=\"1\" z=\"1\" />\n        <TransformUnk x=\"0\" y=\"0\" z=\"0\" w=\"0\" />\n      </Item>\n    </Bones>\n  </Skeleton>\n";
    let xml = XML.replace("  <DrawableModelsHigh>", &format!("{skeleton}  <DrawableModelsHigh>"));
    let built = rage_formats::build_ydr_from_xml_checked(&xml, None).unwrap();
    assert_eq!(built.warnings.len(), 2, "{:?}", built.warnings);
    assert_eq!(built.warnings[1], "bone 1: no name");
}

#[test]
fn a_geometry_with_more_vertices_than_a_16_bit_count_is_an_error() {
    let rows = "              0 0 0   0 0 1   255 255 255 255   0 0\n".repeat(65536);
    let open = "            <Data>\n";
    let start = XML.find(open).unwrap() + open.len();
    let end = XML.find("            </Data>\n          </VertexBuffer>").unwrap();
    let xml = format!("{}{rows}{}", &XML[..start], &XML[end..]);
    let err = format!("{:#}", build_ydr_from_xml(&xml, None).unwrap_err());
    assert!(err.contains("65536 vertices in a geometry exceeds 65535"), "{err}");
}

#[test]
fn only_a_drawable_document_builds_a_drawable() {
    let err = format!("{:#}", build_ydr_from_xml(include_str!("fixtures/composite.ybn.xml"), None).unwrap_err());
    assert!(err.contains("root element is <BoundsFile>, not <Drawable>"), "{err}");
    assert!(rage_formats::build_ybn_from_xml(XML).unwrap_err().to_string().contains("root element is <Drawable>, not <BoundsFile> or <Bounds>"));
}

#[test]
fn an_extra_model_list_next_to_a_lod_list_is_a_clear_error() {
    let m0 = XML.find("  <DrawableModelsHigh>").unwrap();
    let m1 = XML.find("  </DrawableModelsHigh>\n").unwrap() + "  </DrawableModelsHigh>\n".len();
    let high = &XML[m0..m1];
    let extra = high.replace("DrawableModelsHigh", "DrawableModelsX");
    let both = format!("{}{high}{extra}{}", &XML[..m0], &XML[m1..]);
    let err = format!("{:#}", build_ydr_from_xml(&both, None).unwrap_err());
    assert!(err.contains("<DrawableModelsX> cannot be built next to <DrawableModelsHigh>"), "{err}");
    let alone = format!("{}{extra}{}", &XML[..m0], &XML[m1..]);
    let built = rage_formats::build_ydr_from_xml_checked(&alone, None).unwrap();
    assert_eq!(built.xml, alone, "on its own the extra list round-trips");
}
