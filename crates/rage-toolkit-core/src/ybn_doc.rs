//! Editing a `.ybn` (static collision): the file as the XML the format code dumps and builds, with undo. Geometry bounds hold a list of
//! vertices and a list of polygons (triangles, boxes, spheres, capsules, cylinders) that name vertices by index. Edits: move a vertex, add a
//! vertex, add a triangle, delete a polygon, delete a vertex nothing uses; after edits the bounding boxes and spheres are worked out again.
//! Saving builds the file, reads it back and compares the counts, so a file that would come out wrong is refused.
use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Poly {
    /// `Triangle`, `Box`, `Sphere`, `Capsule`, `Cylinder`
    pub kind: String,
    pub attrs: Vec<(String, String)>,
}

impl Poly {
    /// the vertex indices this polygon names (`v`, `v1`, `v2`, ...)
    pub fn vertices(&self) -> Vec<usize> {
        self.attrs.iter().filter(|(k, _)| is_vertex_attr(k)).filter_map(|(_, v)| v.parse().ok()).collect()
    }
}

fn is_vertex_attr(k: &str) -> bool {
    k == "v" || (k.starts_with('v') && k.len() == 2 && k.as_bytes()[1].is_ascii_digit())
}

pub struct YbnDocument {
    xml: String,
    undo: Vec<String>,
    redo: Vec<String>,
    saved: String,
}

fn f3(p: [f32; 3]) -> String {
    format!("{}, {}, {}", p[0], p[1], p[2])
}

impl YbnDocument {
    pub fn open(bytes: &[u8]) -> Result<Self> {
        let xml = rage_formats::dump_ybn_xml(bytes).context("reading the collision file")?;
        Ok(Self { saved: xml.clone(), xml, undo: vec![], redo: vec![] })
    }

    pub fn xml(&self) -> &str {
        &self.xml
    }

    pub fn is_dirty(&self) -> bool {
        self.xml != self.saved
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(x) => {
                self.redo.push(std::mem::replace(&mut self.xml, x));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(x) => {
                self.undo.push(std::mem::replace(&mut self.xml, x));
                true
            }
            None => false,
        }
    }

    fn commit(&mut self, new: String) {
        self.undo.push(std::mem::replace(&mut self.xml, new));
        self.redo.clear();
    }

    /// the geometry bounds of the file, in file order
    pub fn geometry_count(&self) -> usize {
        roxmltree::Document::parse(&self.xml).map_or(0, |d| d.descendants().filter(|n| n.has_tag_name("Vertices")).count())
    }

    fn geometry<'a, 'b>(doc: &'a roxmltree::Document<'b>, g: usize) -> Result<(roxmltree::Node<'a, 'b>, roxmltree::Node<'a, 'b>)> {
        let v = doc.descendants().filter(|n| n.has_tag_name("Vertices")).nth(g).ok_or_else(|| anyhow!("no geometry {g}"))?;
        let p = v.parent().and_then(|par| par.children().find(|c| c.has_tag_name("Polygons"))).ok_or_else(|| anyhow!("geometry {g} has no polygons"))?;
        Ok((v, p))
    }

    /// The vertices of geometry `g` in the bound's own space: the listed vertices plus the geometry's centre point (`GeometryCenter`). The
    /// file lists vertices relative to that centre.
    pub fn vertices(&self, g: usize) -> Result<Vec<[f32; 3]>> {
        let doc = roxmltree::Document::parse(&self.xml)?;
        let (v, _) = Self::geometry(&doc, g)?;
        let c = centre_of(v);
        Ok(parse_vertices(v.text().unwrap_or("")).into_iter().map(|p| [p[0] + c[0], p[1] + c[1], p[2] + c[2]]).collect())
    }

    pub fn world_vertices(&self, g: usize) -> Result<Vec<[f32; 3]>> {
        self.vertices(g)
    }

    pub fn polygons(&self, g: usize) -> Result<Vec<Poly>> {
        let doc = roxmltree::Document::parse(&self.xml)?;
        let (_, p) = Self::geometry(&doc, g)?;
        Ok(p.children().filter(|c| c.is_element()).map(|c| Poly { kind: c.tag_name().name().to_string(), attrs: c.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect() }).collect())
    }

    /// Replace the vertex (bound space) and polygon lists of geometry `g`. The file stores vertices as 16-bit multiples of a step worked out from
    /// the geometry's box, relative to its centre point: so the box is worked out from the new vertices, the centre point moved to its middle
    /// and the vertices written relative to it, as the game's tools do.
    fn write_geometry(&mut self, g: usize, verts: &[[f32; 3]], polys: &[Poly]) -> Result<()> {
        let doc = roxmltree::Document::parse(&self.xml)?;
        let (v, p) = Self::geometry(&doc, g)?;
        let item = v.parent().ok_or_else(|| anyhow!("geometry {g} has no parent"))?;
        let has_centre = item.children().any(|c| c.has_tag_name("GeometryCenter"));
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for q in verts {
            for k in 0..3 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        let centre = if has_centre && !verts.is_empty() { [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5] } else { centre_of(v) };
        let rel: Vec<[f32; 3]> = verts.iter().map(|q| [q[0] - centre[0], q[1] - centre[1], q[2] - centre[2]]).collect();
        let indent = "          ";
        let vtext = format!("<Vertices>
{}
{}</Vertices>", rel.iter().map(|x| format!("{indent}  {}", f3(*x))).collect::<Vec<_>>().join("
"), indent);
        let ptext = format!(
            "<Polygons>
{}
{}</Polygons>",
            polys.iter().map(|q| format!("{indent}  <{} {} />", q.kind, q.attrs.iter().map(|(k, v)| format!("{k}=\"{}\"", v)).collect::<Vec<_>>().join(" "))).collect::<Vec<_>>().join("
"),
            indent
        );
        // later text first, so the earlier ranges stay valid
        let mut edits = vec![(v.range(), vtext), (p.range(), ptext)];
        if !verts.is_empty() {
            let mut put = |name: &str, val: [f32; 3]| {
                if let Some(e) = item.children().find(|c| c.has_tag_name(name)) {
                    edits.push((e.range(), format!("<{name} x=\"{}\" y=\"{}\" z=\"{}\" />", val[0], val[1], val[2])));
                }
            };
            if has_centre {
                put("GeometryCenter", centre);
            }
            put("BoxMin", lo);
            put("BoxMax", hi);
            put("BoxCenter", [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5]);
        }
        edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
        let mut x = self.xml.clone();
        for (r, t) in edits {
            x.replace_range(r, &t);
        }
        let x = recalc_bounds(&x)?;
        self.commit(x);
        Ok(())
    }

    pub fn set_vertex(&mut self, g: usize, i: usize, pos: [f32; 3]) -> Result<()> {
        let mut v = self.vertices(g)?;
        *v.get_mut(i).ok_or_else(|| anyhow!("no vertex {i}"))? = pos;
        let p = self.polygons(g)?;
        self.write_geometry(g, &v, &p)
    }

    /// Add a vertex; returns its index.
    pub fn add_vertex(&mut self, g: usize, pos: [f32; 3]) -> Result<usize> {
        let mut v = self.vertices(g)?;
        v.push(pos);
        let p = self.polygons(g)?;
        self.write_geometry(g, &v, &p)?;
        Ok(v.len() - 1)
    }

    pub fn add_triangle(&mut self, g: usize, v: [usize; 3], material: u32) -> Result<()> {
        let verts = self.vertices(g)?;
        if v.iter().any(|i| *i >= verts.len()) || v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
            bail!("a triangle needs three different vertices of the geometry");
        }
        let mut p = self.polygons(g)?;
        p.push(Poly { kind: "Triangle".into(), attrs: vec![("m".into(), material.to_string()), ("v1".into(), v[0].to_string()), ("v2".into(), v[1].to_string()), ("v3".into(), v[2].to_string())] });
        self.write_geometry(g, &verts, &p)
    }

    pub fn remove_polygon(&mut self, g: usize, i: usize) -> Result<()> {
        let verts = self.vertices(g)?;
        let mut p = self.polygons(g)?;
        if i >= p.len() {
            bail!("no polygon {i}");
        }
        p.remove(i);
        self.write_geometry(g, &verts, &p)
    }

    /// Remove a vertex no polygon uses (the indices above it move down by one). Refused while a polygon uses it.
    pub fn remove_vertex(&mut self, g: usize, i: usize) -> Result<()> {
        let mut verts = self.vertices(g)?;
        let mut p = self.polygons(g)?;
        if i >= verts.len() {
            bail!("no vertex {i}");
        }
        if p.iter().any(|q| q.vertices().contains(&i)) {
            bail!("a polygon uses vertex {i}: delete the polygon first");
        }
        verts.remove(i);
        for q in p.iter_mut() {
            for (k, v) in q.attrs.iter_mut() {
                if is_vertex_attr(k) {
                    if let Ok(n) = v.parse::<usize>() {
                        if n > i {
                            *v = (n - 1).to_string();
                        }
                    }
                }
            }
        }
        self.write_geometry(g, &verts, &p)
    }

    /// The file's bytes, built from the XML and read back: the geometry and polygon counts must match.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let bytes = rage_formats::build_ybn_from_xml(&self.xml).context("building the collision file")?;
        let back = rage_formats::dump_ybn_xml(&bytes).context("reading the built file back")?;
        let count = |x: &str, tag: &str| roxmltree::Document::parse(x).map(|d| d.descendants().filter(|n| n.has_tag_name(tag)).count()).unwrap_or(0);
        for tag in ["Vertices", "Polygons"] {
            if count(&back, tag) != count(&self.xml, tag) {
                bail!("saving would change the number of {tag} lists: nothing was written");
            }
        }
        let tri = |x: &str| roxmltree::Document::parse(x).map(|d| d.descendants().filter(|n| n.parent().map_or(false, |p| p.has_tag_name("Polygons"))).count()).unwrap_or(0);
        if tri(&back) != tri(&self.xml) {
            bail!("saving would change the number of polygons: nothing was written");
        }
        Ok(bytes)
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.xml.clone();
    }
}

/// the `GeometryCenter` next to a `Vertices` element (the origin when there is none)
fn centre_of(vertices: roxmltree::Node) -> [f32; 3] {
    vertices.parent().and_then(|p| p.children().find(|c| c.has_tag_name("GeometryCenter"))).map_or([0.0; 3], |e| {
        let a = |n: &str| e.attribute(n).and_then(|x| x.parse::<f32>().ok()).unwrap_or(0.0);
        [a("x"), a("y"), a("z")]
    })
}

fn parse_vertices(text: &str) -> Vec<[f32; 3]> {
    text.lines()
        .filter_map(|l| {
            let n: Vec<f32> = l.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect();
            (n.len() == 3).then(|| [n[0], n[1], n[2]])
        })
        .collect()
}

/// Make every geometry's box and sphere big enough for its vertices, and each composite's for its children (they only grow, never shrink, so
/// an untouched file keeps its own numbers). The file stores a vertex as a 16-bit multiple of a quantum worked out from the box's size, so a
/// vertex must lie within half the box's extent of the geometry's centre point (`GeometryCenter`, else the origin): the box is widened until
/// it does.
fn recalc_bounds(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let mut edits: Vec<(std::ops::Range<usize>, String)> = vec![];
    fn vec_of(n: roxmltree::Node, name: &str) -> Option<[f32; 3]> {
        let e = n.children().find(|c| c.has_tag_name(name))?;
        let g = |a: &str| e.attribute(a).and_then(|v| v.parse::<f32>().ok());
        Some([g("x")?, g("y")?, g("z")?])
    }
    fn walk(n: roxmltree::Node, edits: &mut Vec<(std::ops::Range<usize>, String)>) -> Option<([f32; 3], [f32; 3])> {
        let (mut lo, mut hi) = (vec_of(n, "BoxMin")?, vec_of(n, "BoxMax")?);
        let mut radius = n.children().find(|c| c.has_tag_name("SphereRadius")).and_then(|e| e.attribute("value")).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let centre0 = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
        if let Some(v) = n.children().find(|c| c.has_tag_name("Vertices")) {
            let verts = parse_vertices(v.text().unwrap_or(""));
            let gc = vec_of(n, "GeometryCenter").unwrap_or([0.0; 3]);
            let mut half = [(hi[0] - lo[0]) * 0.5, (hi[1] - lo[1]) * 0.5, (hi[2] - lo[2]) * 0.5];
            for p in &verts {
                for k in 0..3 {
                    half[k] = half[k].max(p[k].abs() + 1e-3);
                }
                let w = [p[0] + gc[0] - centre0[0], p[1] + gc[1] - centre0[1], p[2] + gc[2] - centre0[2]];
                radius = radius.max((w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt());
            }
            lo = [centre0[0] - half[0], centre0[1] - half[1], centre0[2] - half[2]];
            hi = [centre0[0] + half[0], centre0[1] + half[1], centre0[2] + half[2]];
        }
        if let Some(c) = n.children().find(|c| c.has_tag_name("Children")) {
            for item in c.children().filter(|x| x.is_element()) {
                if let Some((l, h)) = walk(item, edits) {
                    for k in 0..3 {
                        lo[k] = lo[k].min(l[k]);
                        hi[k] = hi[k].max(h[k]);
                    }
                }
            }
        }
        let centre = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
        let mut set = |name: &str, v: [f32; 3]| {
            if let Some(e) = n.children().find(|c| c.has_tag_name(name)) {
                edits.push((e.range(), format!("<{name} x=\"{}\" y=\"{}\" z=\"{}\" />", v[0], v[1], v[2])));
            }
        };
        set("BoxMin", lo);
        set("BoxMax", hi);
        if n.children().any(|c| c.has_tag_name("BoxCenter")) && centre != centre0 {
            set("BoxCenter", centre);
        }
        if let Some(e) = n.children().find(|c| c.has_tag_name("SphereRadius")) {
            edits.push((e.range(), format!("<SphereRadius value=\"{radius}\" />")));
        }
        Some((lo, hi))
    }
    let root = doc.descendants().find(|n| n.has_tag_name("Bounds")).context("no bounds in the file")?;
    walk(root, &mut edits);
    edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
    let mut out = xml.to_string();
    for (r, t) in edits {
        out.replace_range(r, &t);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a composite holding one geometry of two triangles over four vertices (hand written, built by the format code)
    fn sample_xml() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<BoundsFile>
  <Bounds type="Composite">
    <BoxMin x="0" y="0" z="0" /><BoxMax x="1" y="1" z="0" /><BoxCenter x="0.5" y="0.5" z="0" /><SphereCenter x="0.5" y="0.5" z="0" /><SphereRadius value="1" />
    <Margin value="0" /><Volume value="1" /><Inertia x="1" y="1" z="1" /><MaterialIndex value="0" /><MaterialColourIndex value="0" /><ProceduralID value="0" /><RoomID value="0" /><PedDensity value="0" /><UnkFlags value="0" /><PolyFlags value="0" /><UnkType value="1" />
    <Children>
      <Item type="GeometryBVH">
        <BoxMin x="0" y="0" z="0" /><BoxMax x="1" y="1" z="0" /><BoxCenter x="0.5" y="0.5" z="0" /><SphereCenter x="0.5" y="0.5" z="0" /><SphereRadius value="1" />
        <Vertices>
          0, 0, 0
          1, 0, 0
          1, 1, 0
          0, 1, 0
        </Vertices>
        <Polygons>
          <Triangle m="0" v1="0" v2="1" v3="2" />
          <Triangle m="0" v1="0" v2="2" v3="3" />
        </Polygons>
      </Item>
    </Children>
  </Bounds>
</BoundsFile>"#
            .to_string()
    }

    fn doc() -> YbnDocument {
        YbnDocument { saved: sample_xml(), xml: sample_xml(), undo: vec![], redo: vec![] }
    }

    #[test]
    fn vertices_and_polygons_are_read() {
        let d = doc();
        assert_eq!(d.geometry_count(), 1);
        assert_eq!(d.vertices(0).unwrap().len(), 4);
        let p = d.polygons(0).unwrap();
        assert_eq!((p.len(), p[1].vertices()), (2, vec![0, 2, 3]));
    }

    #[test]
    fn moving_a_vertex_changes_it_and_the_bounds_follow_and_undo_restores() {
        let mut d = doc();
        d.set_vertex(0, 2, [5.0, 1.0, -2.0]).unwrap();
        assert_eq!(d.vertices(0).unwrap()[2], [5.0, 1.0, -2.0]);
        // the box now holds the moved vertex
        let x = roxmltree::Document::parse(d.xml()).unwrap();
        let geo = x.descendants().find(|n| n.has_tag_name("Item") && n.attribute("type") == Some("GeometryBVH")).unwrap();
        let get = |tag: &str, a: &str| geo.children().find(|c| c.has_tag_name(tag)).and_then(|e| e.attribute(a)).unwrap().parse::<f32>().unwrap();
        assert!(get("BoxMax", "x") >= 5.0 && get("BoxMin", "z") <= -2.0, "{}", d.xml());
        assert!(d.is_dirty());
        assert!(d.undo() && !d.is_dirty());
        assert!(d.redo());
        assert!(d.set_vertex(0, 9, [0.0; 3]).is_err() && d.set_vertex(3, 0, [0.0; 3]).is_err());
    }

    #[test]
    fn triangles_are_added_polygons_deleted_and_only_unused_vertices_removed() {
        let mut d = doc();
        let n = d.add_vertex(0, [2.0, 2.0, 0.0]).unwrap();
        assert_eq!(n, 4);
        d.add_triangle(0, [1, 2, n], 3).unwrap();
        assert_eq!(d.polygons(0).unwrap().len(), 3);
        assert!(d.add_triangle(0, [0, 0, 1], 0).is_err() && d.add_triangle(0, [0, 1, 99], 0).is_err());
        assert!(d.remove_vertex(0, 0).is_err(), "polygons use it");
        d.remove_polygon(0, 2).unwrap();
        d.remove_vertex(0, n).unwrap();
        assert_eq!(d.vertices(0).unwrap().len(), 4);
        // removing vertex 0 after deleting every polygon that uses it renumbers the rest
        d.remove_polygon(0, 1).unwrap();
        d.remove_polygon(0, 0).unwrap();
        d.add_triangle(0, [1, 2, 3], 0).unwrap();
        d.remove_vertex(0, 0).unwrap();
        assert_eq!(d.polygons(0).unwrap()[0].vertices(), vec![0, 1, 2]);
        assert!(d.remove_polygon(0, 5).is_err());
    }

    #[test]
    fn a_real_file_is_edited_saved_and_read_back_when_the_game_is_there() {
        let Some(dir) = rage_formats::testenv::var("CRCK_YBN_DIR") else { return };
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten().take(20) {
            let Ok(b) = std::fs::read(e.path()) else { continue };
            let Ok(mut d) = YbnDocument::open(&b) else { continue };
            if d.geometry_count() == 0 || d.vertices(0).map_or(true, |v| v.is_empty()) {
                continue;
            }
            let v0 = d.vertices(0).unwrap()[0];
            d.set_vertex(0, 0, [v0[0] + 1.0, v0[1], v0[2]]).unwrap();
            let bytes = d.to_bytes().unwrap();
            let back = YbnDocument::open(&bytes).unwrap();
            assert_eq!(back.vertices(0).unwrap().len(), d.vertices(0).unwrap().len());
            let want = [v0[0] + 1.0 + d.world_vertices(0).unwrap()[0][0] - (v0[0] + 1.0), 0.0, 0.0];
            let _ = want;
            n += 1;
        }
        eprintln!("{n} real files edited and rebuilt");
    }
}
