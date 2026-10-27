//! `.ydr` entry points, ported from the reference tool's `YdrFile` (`YdrXml.GetXml`, `XmlYdr.GetYdr`):
//! read a drawable resource into a block graph, write one back (resource version 165), and
//! convert between a `.ydr` and its XML.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::bounds::BoundBlock;
use super::drawable::Drawable;
use super::xml::XmlOut;
use super::{BlockId, Built, Graph, Reader};
use crate::names::NameTable;
use crate::resource::SYSTEM_BASE;

/// Reads a `.ydr` into a graph; returns it with the [`Drawable`] root.
pub fn read_ydr(file: &[u8]) -> Result<(Graph, BlockId)> {
    let mut r = Reader::open(file)?;
    let mut g = Graph::new();
    let root = Drawable::read(&mut r, &mut g, SYSTEM_BASE)?.context("the resource has no root block")?;
    Ok((g, root))
}

/// Lays the graph out and writes it as a `.ydr` (resource version 165).
pub fn write_ydr(g: &mut Graph, root: BlockId) -> Result<Vec<u8>> {
    let (pages, bound) = { let d = g.get::<Drawable>(root); (d.pages.context("the drawable has no pages info")?, d.bound) };
    if let Some(bound) = bound { BoundBlock::prepare_tree(g, bound); }
    g.build(root, pages, 165)
}

/// `YdrXml.GetXml`: the `<Drawable>` document.
pub fn xml_of(g: &Graph, root: BlockId, names: &NameTable, dds_dir: Option<&Path>) -> Result<String> {
    let mut x = XmlOut::new();
    x.open("Drawable");
    g.get::<Drawable>(root).write_xml(&mut x, g, names, dds_dir)?;
    x.close("Drawable");
    Ok(x.out)
}

/// A `.ydr` as the reference tool's XML; embedded textures are written to `dds_dir` when given.
pub fn dump_ydr_xml(file: &[u8], names: &NameTable, dds_dir: Option<&Path>) -> Result<String> {
    let (g, root) = read_ydr(file)?;
    xml_of(&g, root, names, dds_dir)
}

/// The `<Drawable>` document read into a graph; anything else at the root is an error.
fn read_ydr_xml(doc: &roxmltree::Document, dds_dir: Option<&Path>, warnings: &mut Vec<String>) -> Result<(Graph, BlockId)> {
    let node = doc.root_element();
    let tag = node.tag_name().name();
    if tag != "Drawable" { bail!("the XML's root element is <{tag}>, not <Drawable>: it is not a drawable"); }
    let mut g = Graph::new();
    let root = Drawable::read_xml(node, &mut g, dds_dir, warnings)?;
    Ok((g, root))
}

/// `XmlYdr.GetYdr` then a write: the `.ydr` for a `<Drawable>` document.
pub fn build_ydr_from_xml(xml: &str, dds_dir: Option<&Path>) -> Result<Vec<u8>> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let (mut g, root) = read_ydr_xml(&doc, dds_dir, &mut Vec::new())?;
    write_ydr(&mut g, root)
}

/// [`build_ydr_from_xml`], then reads the bytes back and compares their XML with that of the written graph
/// (taken after the write, which orders BVH polygons). Returns the file, that XML and the warnings for what
/// The reference tool accepts silently (a texture that is not embedded, a bone without a name); a file that does not
/// read back identically is an error, never handed over. The comparison is of XML dumped with no DDS folder,
/// so texture pixels are outside it.
pub fn build_ydr_from_xml_checked(xml: &str, dds_dir: Option<&Path>) -> Result<Built> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let mut warnings = Vec::new();
    let (mut g, root) = read_ydr_xml(&doc, dds_dir, &mut warnings)?;
    let bytes = write_ydr(&mut g, root)?;
    let names = NameTable::core();
    let expected = xml_of(&g, root, &names, None)?;
    let back = dump_ydr_xml(&bytes, &names, None).context("the written file cannot be read back")?;
    if let Some(msg) = super::xml::first_difference(&expected, &back) {
        bail!("the written file does not read back identically: {msg}");
    }
    Ok(Built { bytes, xml: expected, warnings })
}
