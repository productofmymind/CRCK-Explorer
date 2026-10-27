//! The water surface (`water.xml`, `water_heistisland.xml`): axis-aligned
//! quads at a height, plus the calming and wave quads that shape the
//! surface. The game keeps the first under `common/data/levels/gta5/` and
//! the island one under `update.rpf`'s copy of that folder. Read as
//! The reference library's `World/Water.cs` reads them; only the water quads are
//! ever rendered there.

use anyhow::{Context, Result};
use roxmltree::Node;

/// A `WaterQuads/Item`: a rectangle of water at `z`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterQuad {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
    pub kind: i32,
    pub invisible: bool,
    pub limited_depth: bool,
    pub z: f32,
    /// Per-corner alpha, as stored.
    pub alpha: [f32; 4],
    pub no_stencil: bool,
}

/// A `CalmingQuads/Item`: where waves are damped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalmingQuad {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
    pub dampening: f32,
}

/// A `WaveQuads/Item`: a wave train's amplitude and direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveQuad {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
    pub amplitude: f32,
    pub x_direction: f32,
    pub y_direction: f32,
}

/// Everything one water file declares.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaterData {
    pub quads: Vec<WaterQuad>,
    pub calming_quads: Vec<CalmingQuad>,
    pub wave_quads: Vec<WaveQuad>,
}

impl WaterQuad {
    /// Whether the quad's rectangle meets `x0,y0,x1,y1` (edges included).
    pub fn intersects(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        self.min_x <= x1 && self.max_x >= x0 && self.min_y <= y1 && self.max_y >= y0
    }
}

/// `<name value="..."/>` under `node`, as `Xml.GetChild*Attribute(node,
/// name, "value")` reads it: missing means the type's zero.
fn value<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name).and_then(|c| c.attribute("value"))
}

fn float(node: Node, name: &str) -> f32 {
    value(node, name).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
}

fn int(node: Node, name: &str) -> i32 {
    value(node, name).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

fn boolean(node: Node, name: &str) -> bool {
    value(node, name).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
}

/// The `<Item>` children of `<list>` under the root.
fn items<'a>(root: Node<'a, 'a>, list: &str) -> Vec<Node<'a, 'a>> {
    root.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == list)
        .flat_map(|l| l.children().filter(|c| c.is_element() && c.tag_name().name() == "Item"))
        .collect()
}

/// Parses a `water*.xml` document.
pub fn parse_water_xml(text: &str) -> Result<WaterData> {
    let doc = roxmltree::Document::parse(text).context("water.xml")?;
    let root = doc.root_element();
    let mut out = WaterData::default();
    for item in items(root, "WaterQuads") {
        out.quads.push(WaterQuad {
            min_x: float(item, "minX"),
            max_x: float(item, "maxX"),
            min_y: float(item, "minY"),
            max_y: float(item, "maxY"),
            kind: int(item, "Type"),
            invisible: boolean(item, "IsInvisible"),
            limited_depth: boolean(item, "HasLimitedDepth"),
            z: float(item, "z"),
            alpha: [float(item, "a1"), float(item, "a2"), float(item, "a3"), float(item, "a4")],
            no_stencil: boolean(item, "NoStencil"),
        });
    }
    for item in items(root, "CalmingQuads") {
        out.calming_quads.push(CalmingQuad {
            min_x: float(item, "minX"),
            max_x: float(item, "maxX"),
            min_y: float(item, "minY"),
            max_y: float(item, "maxY"),
            dampening: float(item, "fDampening"),
        });
    }
    for item in items(root, "WaveQuads") {
        out.wave_quads.push(WaveQuad {
            min_x: float(item, "minX"),
            max_x: float(item, "maxX"),
            min_y: float(item, "minY"),
            max_y: float(item, "maxY"),
            amplitude: float(item, "Amplitude"),
            x_direction: float(item, "XDirection"),
            y_direction: float(item, "YDirection"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<WaterData>
  <WaterQuads>
    <Item>
      <minX value="-1592" />
      <maxX value="-1304" />
      <minY value="-1744" />
      <maxY value="-1624" />
      <Type value="0" />
      <IsInvisible value="false" />
      <HasLimitedDepth value="false" />
      <z value="0.0" />
      <a1 value="26" />
      <a2 value="26" />
      <a3 value="26" />
      <a4 value="26" />
      <NoStencil value="false" />
    </Item>
    <Item>
      <minX value="10" />
      <maxX value="20" />
      <minY value="30" />
      <maxY value="40" />
      <Type value="2" />
      <IsInvisible value="true" />
      <HasLimitedDepth value="true" />
      <z value="12.5" />
      <a1 value="1" />
      <a2 value="2" />
      <a3 value="3" />
      <a4 value="4" />
      <NoStencil value="true" />
    </Item>
  </WaterQuads>
  <CalmingQuads>
    <Item>
      <minX value="1752" />
      <maxX value="2076" />
      <minY value="216" />
      <maxY value="800" />
      <fDampening value="0.05" />
    </Item>
  </CalmingQuads>
  <WaveQuads>
    <Item>
      <minX value="1664" />
      <maxX value="1988" />
      <minY value="-120" />
      <maxY value="132" />
      <Amplitude value="0.1" />
      <XDirection value="-0.603208" />
      <YDirection value="-0.797584" />
    </Item>
  </WaveQuads>
</WaterData>"#;

    #[test]
    fn reads_the_three_quad_lists() {
        let water = parse_water_xml(SAMPLE).unwrap();
        assert_eq!(water.quads.len(), 2);
        let q = water.quads[0];
        assert_eq!((q.min_x, q.max_x, q.min_y, q.max_y, q.z), (-1592.0, -1304.0, -1744.0, -1624.0, 0.0));
        assert!(!q.invisible && !q.limited_depth && !q.no_stencil && q.kind == 0);
        assert_eq!(q.alpha, [26.0; 4]);
        let q = water.quads[1];
        assert!(q.invisible && q.limited_depth && q.no_stencil && q.kind == 2 && q.z == 12.5);
        assert_eq!(q.alpha, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(water.calming_quads, vec![CalmingQuad { min_x: 1752.0, max_x: 2076.0, min_y: 216.0, max_y: 800.0, dampening: 0.05 }]);
        assert_eq!(water.wave_quads.len(), 1);
        assert_eq!(water.wave_quads[0].x_direction, -0.603208);
    }

    #[test]
    fn a_quad_meets_a_box_on_its_edge() {
        let q = parse_water_xml(SAMPLE).unwrap().quads[1];
        assert!(q.intersects(0.0, 0.0, 10.0, 30.0), "touching the corner counts");
        assert!(q.intersects(15.0, 35.0, 16.0, 36.0), "inside");
        assert!(!q.intersects(21.0, 0.0, 30.0, 100.0));
        assert!(!q.intersects(0.0, 41.0, 100.0, 50.0));
    }

    #[test]
    fn an_empty_document_has_no_water() {
        let water = parse_water_xml("<WaterData/>").unwrap();
        assert_eq!(water, WaterData::default());
        assert!(parse_water_xml("<WaterData>").is_err());
    }
}
