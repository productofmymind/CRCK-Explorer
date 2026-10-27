//! Collision bounds (`.ybn`), read only: the `phBound` tree a static map
//! piece or an MLO streams for physics. A file is one root bound, usually a
//! `phBoundComposite` whose children are `phBoundBVH`/`phBoundGeometry`
//! meshes (quantised vertices plus 16-byte polygon records), each placed by
//! a per-child transform. Only triangles are decoded into world space; the
//! primitive polygons (spheres, capsules, boxes, cylinders) are counted and
//! skipped for now.
//!
//! Byte layout ported from the reference library `Bounds.cs` / `YbnFile.cs`.

use anyhow::{bail, Context, Result};

use crate::math::Vec3;
use crate::resource::{prepare_rsc7, u16_le, u32_le, u64_le, vec3_le, ResReader, SYSTEM_BASE};

/// `phBound` subclass, from the byte at 0x10 of every bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundKind {
    Sphere,
    Capsule,
    Box,
    Geometry,
    GeometryBvh,
    Composite,
    Disc,
    Cylinder,
    Cloth,
    Unknown(u8),
}

impl BoundKind {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Sphere,
            1 => Self::Capsule,
            3 => Self::Box,
            4 => Self::Geometry,
            8 => Self::GeometryBvh,
            10 => Self::Composite,
            12 => Self::Disc,
            13 => Self::Cylinder,
            15 => Self::Cloth,
            other => Self::Unknown(other),
        }
    }
}

/// A `Matrix4F_s`: three basis columns and a translation, applied as
/// `c1*x + c2*y + c3*z + c4`. `None` where the file stores no transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundTransform {
    pub columns: [Vec3; 4],
}

impl BoundTransform {
    pub fn identity() -> Self {
        Self { columns: [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 0.0)] }
    }

    pub fn apply(&self, p: Vec3) -> Vec3 {
        let [c1, c2, c3, c4] = self.columns;
        c1 * p.x + c2 * p.y + c3 * p.z + c4
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }
}

/// One triangle of a geometry bound, indices into [`BoundGeometry::vertices`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundTriangle {
    pub indices: [u16; 3],
    /// Index into [`BoundGeometry::materials`].
    pub material_index: u8,
}

/// A `phBoundGeometry` / `phBoundBVH`: a triangle soup in the bound's local
/// space (already dequantised and offset by `CenterGeom`).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundGeometry {
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<BoundTriangle>,
    /// Polygons that were not triangles (spheres, capsules, boxes, cylinders).
    pub primitive_count: usize,
    /// `BoundMaterial_s.Data1` per material; the low byte is the material type.
    pub materials: Vec<u32>,
}

/// One node of the bound tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    pub kind: BoundKind,
    pub box_min: Vec3,
    pub box_max: Vec3,
    /// Filled for `Geometry`/`GeometryBvh`.
    pub geometry: Option<BoundGeometry>,
    /// Filled for `Composite`: each child with its placement.
    pub children: Vec<(Bound, BoundTransform)>,
}

/// A parsed `.ybn`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ybn {
    pub root: Bound,
}

/// A world-space triangle flattened out of the bound tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Triangle {
    pub vertices: [Vec3; 3],
    /// Material type (low byte of the material's `Data1`).
    pub material: u8,
}

impl Triangle {
    /// Unit normal following the winding of `vertices`.
    pub fn normal(&self) -> Vec3 {
        let [a, b, c] = self.vertices;
        (b - a).cross(c - a).normalize()
    }
}

impl Ybn {
    /// Every triangle in the file, in world space (child transforms applied,
    /// innermost first).
    pub fn triangles(&self) -> Vec<Triangle> {
        let mut out = Vec::new();
        collect_triangles(&self.root, &mut Vec::new(), &mut out);
        out
    }

    /// Every geometry bound in the tree, depth first.
    pub fn geometries(&self) -> Vec<&BoundGeometry> {
        fn walk<'a>(b: &'a Bound, out: &mut Vec<&'a BoundGeometry>) {
            if let Some(g) = &b.geometry { out.push(g); }
            for (child, _) in &b.children { walk(child, out); }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

fn collect_triangles(bound: &Bound, stack: &mut Vec<BoundTransform>, out: &mut Vec<Triangle>) {
    if let Some(g) = &bound.geometry {
        for tri in &g.triangles {
            let mut vertices = [Vec3::new(0.0, 0.0, 0.0); 3];
            let mut valid = true;
            for (slot, &index) in vertices.iter_mut().zip(tri.indices.iter()) {
                match g.vertices.get(index as usize) {
                    Some(&v) => {
                        let mut p = v;
                        for xf in stack.iter().rev() {
                            p = xf.apply(p);
                        }
                        *slot = p;
                    }
                    None => { valid = false; break; }
                }
            }
            if !valid { continue; }
            let material = g.materials.get(tri.material_index as usize).map_or(0, |m| (m & 0xFF) as u8);
            out.push(Triangle { vertices, material });
        }
    }
    for (child, xf) in &bound.children {
        stack.push(*xf);
        collect_triangles(child, stack, out);
        stack.pop();
    }
}

/// Parses a `.ybn` (RSC7 bytes, deflated or stored).
pub fn parse_ybn(data: &[u8]) -> Result<Ybn> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    let root = read_bound(&reader, SYSTEM_BASE, 0).context("root bound")?;
    Ok(Ybn { root })
}

const BOUND_HEADER: usize = 0x70;

fn read_bound(r: &ResReader<'_>, va: u64, depth: usize) -> Result<Bound> {
    if depth > 8 {
        bail!("bound tree nested deeper than 8 levels");
    }
    let head = r.resolve(va, BOUND_HEADER).with_context(|| format!("bound header at 0x{va:X}"))?;
    let kind = BoundKind::from_byte(head[0x10]);
    let box_max = vec3_le(head, 0x20);
    let box_min = vec3_le(head, 0x30);

    let mut bound = Bound { kind, box_min, box_max, geometry: None, children: Vec::new() };
    match kind {
        BoundKind::Geometry | BoundKind::GeometryBvh => {
            bound.geometry = Some(read_geometry(r, va).context("geometry bound")?);
        }
        BoundKind::Composite => {
            bound.children = read_composite_children(r, va, depth).context("composite bound")?;
        }
        _ => {}
    }
    Ok(bound)
}

fn read_geometry(r: &ResReader<'_>, va: u64) -> Result<BoundGeometry> {
    // BoundGeometry is 304 bytes; the fields we need sit between 0x88 and 0x121.
    let b = r.resolve(va, 0x130).context("geometry header")?;
    let polygons_ptr = u64_le(b, 0x88);
    let quantum = vec3_le(b, 0x90);
    let center = vec3_le(b, 0xA0);
    let vertices_ptr = u64_le(b, 0xB0);
    let vertices_count = u32_le(b, 0xD0) as usize;
    let polygons_count = u32_le(b, 0xD4) as usize;
    let materials_ptr = u64_le(b, 0xF0);
    let poly_material_ptr = u64_le(b, 0x118);
    let materials_count = b[0x120] as usize;

    let mut vertices = Vec::with_capacity(vertices_count);
    if vertices_count > 0 {
        let vb = r.resolve(vertices_ptr, vertices_count * 6).context("geometry vertices")?;
        for i in 0..vertices_count {
            let x = i16::from_le_bytes([vb[i * 6], vb[i * 6 + 1]]) as f32;
            let y = i16::from_le_bytes([vb[i * 6 + 2], vb[i * 6 + 3]]) as f32;
            let z = i16::from_le_bytes([vb[i * 6 + 4], vb[i * 6 + 5]]) as f32;
            vertices.push(Vec3::new(x * quantum.x, y * quantum.y, z * quantum.z) + center);
        }
    }

    // The reference tool reads at least four material slots regardless of the count.
    let materials_read = materials_count.max(4);
    let materials = match r.resolve_optional(materials_ptr, materials_read * 8) {
        Some(Some(mb)) => (0..materials_read).map(|i| u32_le(mb, i * 8)).collect(),
        _ => Vec::new(),
    };
    let poly_materials: Vec<u8> = match r.resolve_optional(poly_material_ptr, polygons_count) {
        Some(Some(pm)) => pm.to_vec(),
        _ => Vec::new(),
    };

    let mut triangles = Vec::new();
    let mut primitive_count = 0;
    if polygons_count > 0 {
        let pb = r.resolve(polygons_ptr, polygons_count * 16).context("geometry polygons")?;
        for i in 0..polygons_count {
            let rec = &pb[i * 16..i * 16 + 16];
            match rec[0] & 7 {
                0 => {
                    let indices = [
                        u16_le(rec, 4) & 0x7FFF,
                        u16_le(rec, 6) & 0x7FFF,
                        u16_le(rec, 8) & 0x7FFF,
                    ];
                    let material_index = poly_materials.get(i).copied().unwrap_or(0);
                    triangles.push(BoundTriangle { indices, material_index });
                }
                _ => primitive_count += 1,
            }
        }
    }

    Ok(BoundGeometry { vertices, triangles, primitive_count, materials })
}

fn read_composite_children(r: &ResReader<'_>, va: u64, depth: usize) -> Result<Vec<(Bound, BoundTransform)>> {
    let b = r.resolve(va, 0xB0).context("composite header")?;
    let children_ptr = u64_le(b, 0x70);
    let transforms_ptr = u64_le(b, 0x78);
    let count = u16_le(b, 0xA0) as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let pointers = r.read_u64_list(children_ptr, count).context("composite child pointers")?;
    let transforms = match r.resolve_optional(transforms_ptr, count * 64) {
        Some(Some(tb)) => (0..count).map(|i| {
            let o = i * 64;
            BoundTransform { columns: [vec3_le(tb, o), vec3_le(tb, o + 16), vec3_le(tb, o + 32), vec3_le(tb, o + 48)] }
        }).collect(),
        _ => vec![BoundTransform::identity(); count],
    };

    let mut children = Vec::with_capacity(count);
    for (i, &ptr) in pointers.iter().enumerate() {
        if ptr == 0 {
            continue;
        }
        let child = read_bound(r, ptr, depth + 1).with_context(|| format!("composite child {i}"))?;
        children.push((child, transforms[i]));
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_applies_columns_and_translation() {
        let xf = BoundTransform { columns: [
            Vec3::new(0.0, 1.0, 0.0), Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(10.0, 20.0, 30.0),
        ] };
        let p = xf.apply(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(p, Vec3::new(8.0, 21.0, 33.0));
        assert!(BoundTransform::identity().is_identity());
    }

    #[test]
    fn triangle_normal_points_up_for_ccw_floor() {
        let t = Triangle { vertices: [Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)], material: 0 };
        let n = t.normal();
        assert!((n.z - 1.0).abs() < 1e-6, "{n:?}");
    }

    #[test]
    fn kinds_round_trip_known_bytes() {
        assert_eq!(BoundKind::from_byte(10), BoundKind::Composite);
        assert_eq!(BoundKind::from_byte(8), BoundKind::GeometryBvh);
        assert_eq!(BoundKind::from_byte(99), BoundKind::Unknown(99));
    }
}
