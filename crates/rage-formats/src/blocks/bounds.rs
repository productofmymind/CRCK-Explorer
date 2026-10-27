//! Collision bounds, ported from the reference tool's `Bounds.cs`:
//! `Bounds` (the 112-byte base), `BoundSphere`, `BoundCapsule`, `BoundBox`,
//! `BoundDisc`, `BoundCylinder`, `BoundCloth`, `BoundGeometry` (304 bytes),
//! `BoundBVH` (336 bytes; `BuildBVH` reorders the polygons into BVH node order),
//! `BoundComposite` (176 bytes, with its children's transforms, boxes and flags),
//! the five `BoundPolygon` kinds, `BoundMaterial_s`, `BoundVertex_s` and
//! `BoundGeomOctants`. Only the legacy PC layout is handled. The BVH itself is in [`super::bvh`].
//!
//! The derived arrays of a geometry (materials, edge indices, triangle areas,
//! quantum, shrunk vertices, octants) are computed the way `BoundGeometry.ReadXml`
//! and `GetReferences` compute them; see [`Geometry::prepare`].
//!
//! Vertices are stored as `i16 * Quantum` with no `CenterGeom` added: `Read`
//! (`Bounds.cs:1053`, `1066`) multiplies the quantised vector by `Quantum` and
//! `GetReferences` (`1276`, `1313`) divides by it, so `CenterGeom` only enters
//! through `GetVertexPos` (`1405`), which the derived-data algorithms use.

use anyhow::{bail, Result};

use super::base::{read_pages_info, write_file_base, PointerArray64, RawBytes, StructArray};
use super::bvh::{build_bvh, Bvh, BvhItem};
use super::xml::{
    attr_i32, attr_str, attr_u32, child, child_attr_f32, child_attr_u32, child_text, child_vec3, float, format_flags, items, parse_flags, raw_f32s,
    raw_rgba, raw_vec3s, write_items, Node, XmlOut,
};
use super::*;

/// `BoundsType` (`Bounds.cs:101`); `None` is XML-only and not a kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoundKind { Sphere = 0, Capsule = 1, Box = 3, Geometry = 4, GeometryBvh = 8, Composite = 10, Disc = 12, Cylinder = 13, Cloth = 15 }

impl BoundKind {
    /// `BoundsType.ToString()`.
    pub fn name(self) -> &'static str {
        match self {
            BoundKind::Sphere => "Sphere", BoundKind::Capsule => "Capsule", BoundKind::Box => "Box", BoundKind::Geometry => "Geometry",
            BoundKind::GeometryBvh => "GeometryBVH", BoundKind::Composite => "Composite", BoundKind::Disc => "Disc",
            BoundKind::Cylinder => "Cylinder", BoundKind::Cloth => "Cloth",
        }
    }
    /// The kind named `s` (`Enum.TryParse`: an enum name or its number).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        [BoundKind::Sphere, BoundKind::Capsule, BoundKind::Box, BoundKind::Geometry, BoundKind::GeometryBvh, BoundKind::Composite, BoundKind::Disc, BoundKind::Cylinder, BoundKind::Cloth]
            .into_iter().find(|k| k.name() == s)
            .or_else(|| s.parse::<u8>().ok().and_then(Self::from_byte))
    }
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => BoundKind::Sphere, 1 => BoundKind::Capsule, 3 => BoundKind::Box, 4 => BoundKind::Geometry, 8 => BoundKind::GeometryBvh,
            10 => BoundKind::Composite, 12 => BoundKind::Disc, 13 => BoundKind::Cylinder, 15 => BoundKind::Cloth,
            _ => return None,
        })
    }
    /// The `FileVFT` each class's `ReadXml` sets (`Bounds.cs:554`, `624`, `667`, `756`, `831`, `1260`, `2587`, `3070`);
    /// `BoundCloth` sets none, so 0.
    fn default_vft(self) -> u32 {
        match self {
            BoundKind::Sphere => 1080221960, BoundKind::Capsule => 1080213112, BoundKind::Box => 1080221016, BoundKind::Geometry => 1080226408,
            BoundKind::GeometryBvh => 1080228536, BoundKind::Composite => 1080212136, BoundKind::Disc => 1080229960,
            BoundKind::Cylinder => 1080202872, BoundKind::Cloth => 0,
        }
    }
}

/// `EBoundCompositeFlags` (`Bounds.cs:4592`).
pub const COMPOSITE_FLAG_NAMES: [(&str, u32); 33] = [
    ("NONE", 0), ("UNKNOWN", 1), ("MAP_WEAPON", 1 << 1), ("MAP_DYNAMIC", 1 << 2), ("MAP_ANIMAL", 1 << 3), ("MAP_COVER", 1 << 4),
    ("MAP_VEHICLE", 1 << 5), ("VEHICLE_NOT_BVH", 1 << 6), ("VEHICLE_BVH", 1 << 7), ("VEHICLE_BOX", 1 << 8), ("PED", 1 << 9),
    ("RAGDOLL", 1 << 10), ("ANIMAL", 1 << 11), ("ANIMAL_RAGDOLL", 1 << 12), ("OBJECT", 1 << 13), ("OBJECT_ENV_CLOTH", 1 << 14),
    ("PLANT", 1 << 15), ("PROJECTILE", 1 << 16), ("EXPLOSION", 1 << 17), ("PICKUP", 1 << 18), ("FOLIAGE", 1 << 19),
    ("FORKLIFT_FORKS", 1 << 20), ("TEST_WEAPON", 1 << 21), ("TEST_CAMERA", 1 << 22), ("TEST_AI", 1 << 23), ("TEST_SCRIPT", 1 << 24),
    ("TEST_VEHICLE_WHEEL", 1 << 25), ("GLASS", 1 << 26), ("MAP_RIVER", 1 << 27), ("SMOKE", 1 << 28), ("UNSMASHED", 1 << 29),
    ("MAP_STAIRS", 1 << 30), ("MAP_DEEP_SURFACE", 1 << 31),
];

/// `EBoundMaterialFlags` (`Bounds.cs:5138`).
pub const MATERIAL_FLAG_NAMES: [(&str, u16); 17] = [
    ("NONE", 0), ("FLAG_STAIRS", 1), ("FLAG_NOT_CLIMBABLE", 1 << 1), ("FLAG_SEE_THROUGH", 1 << 2), ("FLAG_SHOOT_THROUGH", 1 << 3),
    ("FLAG_NOT_COVER", 1 << 4), ("FLAG_WALKABLE_PATH", 1 << 5), ("FLAG_NO_CAM_COLLISION", 1 << 6), ("FLAG_SHOOT_THROUGH_FX", 1 << 7),
    ("FLAG_NO_DECAL", 1 << 8), ("FLAG_NO_NAVMESH", 1 << 9), ("FLAG_NO_RAGDOLL", 1 << 10), ("FLAG_VEHICLE_WHEEL", 1 << 11),
    ("FLAG_NO_PTFX", 1 << 12), ("FLAG_TOO_STEEP_FOR_PLAYER", 1 << 13), ("FLAG_NO_NETWORK_SPAWN", 1 << 14),
    ("FLAG_NO_CAM_COLLISION_ALLOW_CLIPPING", 1 << 15),
];

/// The parent a bound is written or read under: only `Bounds.Parent != null` and
/// `Parent.OwnerIsFragment` matter to a child's XML.
pub struct CompositeCtx { pub owner_is_fragment: bool }

// ─── small vector helpers with SharpDX's semantics ──────────────────────────

fn vmul(a: Vec3, b: Vec3) -> Vec3 { Vec3::new(a.x * b.x, a.y * b.y, a.z * b.z) }
fn vdiv(a: Vec3, b: Vec3) -> Vec3 { Vec3::new(a.x / b.x, a.y / b.y, a.z / b.z) }
fn vadd_f(a: Vec3, f: f32) -> Vec3 { Vec3::new(a.x + f, a.y + f, a.z + f) }
/// `Vector3.Normalize`: unchanged below `MathUtil.ZeroTolerance` (1e-6).
fn norm(v: Vec3) -> Vec3 {
    let len = v.length();
    if len > 1e-6 { v * (1.0 / len) } else { v }
}
/// `Vector3.Min`/`Max` as SharpDX writes them (`a < b ? a : b`).
pub(super) fn vmin(a: Vec3, b: Vec3) -> Vec3 {
    let m = |a: f32, b: f32| if a < b { a } else { b };
    Vec3::new(m(a.x, b.x), m(a.y, b.y), m(a.z, b.z))
}
pub(super) fn vmax(a: Vec3, b: Vec3) -> Vec3 {
    let m = |a: f32, b: f32| if a > b { a } else { b };
    Vec3::new(m(a.x, b.x), m(a.y, b.y), m(a.z, b.z))
}

/// SharpDX `Ray.Intersects(ref v1, ref v2, ref v3, out dist)` (Moeller-Trumbore).
fn ray_triangle(pos: Vec3, dir: Vec3, v1: Vec3, v2: Vec3, v3: Vec3) -> Option<f32> {
    let e1 = v2 - v1;
    let e2 = v3 - v1;
    let dce2 = dir.cross(e2);
    let det = e1.dot(dce2);
    if det.abs() < 1e-6 { return None; }
    let inv = 1.0 / det;
    let dv = pos - v1;
    let u = dv.dot(dce2) * inv;
    if !(0.0..=1.0).contains(&u) { return None; }
    let dce1 = dv.cross(e1);
    let v = dir.dot(dce1) * inv;
    if v < 0.0 || u + v > 1.0 { return None; }
    let t = e2.dot(dce1) * inv;
    if t < 0.0 { return None; }
    Some(t)
}

/// `TriangleMath.AreaPart`: the area from one corner, and the angle there.
fn area_part(v1: Vec3, v2: Vec3, v3: Vec3) -> (f32, f32) {
    let va = v2 - v1;
    let vb = v3 - v1;
    let (na, nb) = (norm(va), norm(vb));
    let (a, b) = (va.length(), vb.length());
    let c = (na.dot(nb) as f64).acos();
    let area = (0.5 * a as f64 * b as f64 * c.sin()) as f32;
    (area, c.abs() as f32)
}

/// `TriangleMath.Area`: the area computed at the corner whose angle is closest to a right angle's opposite.
fn triangle_area(v1: Vec3, v2: Vec3, v3: Vec3) -> f32 {
    let (a1, t1) = area_part(v1, v2, v3);
    let (a2, t2) = area_part(v2, v3, v1);
    let (a3, t3) = area_part(v3, v1, v2);
    let fp = std::f32::consts::PI;
    let d = |t: f32| t.min((t - fp).abs());
    let (d1, d2, d3) = (d(t1), d(t2), d(t3));
    if d1 >= d2 && a1 != 0.0 {
        if d1 >= d3 || a3 == 0.0 { a1 } else { a3 }
    } else if d2 >= d3 || a3 == 0.0 { a2 } else { a3 }
}

/// `BoundingBox.Transform(Matrix)` as the reference tool's extension writes it (`Vectors.cs:133`): the box's centre
/// through the matrix and its half-extent through the matrix of absolute values.
fn transform_box(min: Vec3, max: Vec3, m: &Mat4) -> (Vec3, Vec3) {
    let abs = Mat4(m.0.map(f32::abs));
    let centre = (max + min) * 0.5;
    let extent = (max - min) * 0.5;
    let p = m.transform_point(centre);
    let inv = 1.0 / p.w; // `TransformCoordinate`
    let ncentre = Vec3::new(p.x * inv, p.y * inv, p.z * inv);
    let nextent = abs.transform_vector(extent).abs();
    (ncentre - nextent, ncentre + nextent)
}

/// `Matrix4F_s` (64 bytes): the matrix's rows without their fourth column, each followed by a flags word.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChildTransform { m: Mat4, flags: [u32; 4] }

impl Pod for ChildTransform {
    const SIZE: usize = 64;
    fn write(&self, w: &mut Writer) { for r in 0..4 { for c in 0..3 { w.f32(self.m.0[r * 4 + c]); } w.u32(self.flags[r]); } }
    fn read(b: &[u8]) -> Self {
        let mut m = Mat4::identity();
        let mut flags = [0u32; 4];
        for r in 0..4 {
            for c in 0..3 { m.0[r * 4 + c] = f32::read(&b[(r * 4 + c) * 4..]); }
            flags[r] = u32::read(&b[(r * 4 + 3) * 4..]);
        }
        ChildTransform { m, flags }
    }
}

/// `AABB_s` (32 bytes): a child's box; `min.w` and `max.w` carry `float.Epsilon` and the child's margin.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Aabb { min: Vec4, max: Vec4 }

impl Pod for Aabb {
    const SIZE: usize = 32;
    fn write(&self, w: &mut Writer) { w.vec4(self.min); w.vec4(self.max); }
    fn read(b: &[u8]) -> Self { Aabb { min: Vec4::read(b), max: Vec4::read(&b[16..]) } }
}

/// `BoundCompositeChildrenFlags` (8 bytes): `Flags1` and `Flags2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChildFlags(u32, u32);

impl Pod for ChildFlags {
    const SIZE: usize = 8;
    fn write(&self, w: &mut Writer) { w.u32(self.0); w.u32(self.1); }
    fn read(b: &[u8]) -> Self { ChildFlags(u32::read(b), u32::read(&b[4..])) }
}

// ─── materials, vertices, colours ───────────────────────────────────────────

/// `BoundMaterial_s` (8 bytes): `Data1` holds type (8 bits), procedural id (8), room id (5), ped density (3)
/// and the low byte of the flags; `Data2` the high byte of the flags, the material colour index and `Unk4`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct BoundMaterial { pub data1: u32, pub data2: u32 }

impl BoundMaterial {
    pub fn type_(&self) -> u8 { self.data1 as u8 }
    pub fn procedural_id(&self) -> u8 { (self.data1 >> 8) as u8 }
    pub fn room_id(&self) -> u8 { ((self.data1 >> 16) & 0x1F) as u8 }
    pub fn ped_density(&self) -> u8 { ((self.data1 >> 21) & 0x7) as u8 }
    pub fn flags(&self) -> u16 { (((self.data1 >> 24) & 0xFF) | ((self.data2 & 0xFF) << 8)) as u16 }
    pub fn material_colour_index(&self) -> u8 { (self.data2 >> 8) as u8 }
    pub fn unk4(&self) -> u16 { (self.data2 >> 16) as u16 }
    pub fn set_type(&mut self, v: u8) { self.data1 = (self.data1 & 0xFFFF_FF00) | v as u32; }
    pub fn set_procedural_id(&mut self, v: u8) { self.data1 = (self.data1 & 0xFFFF_00FF) | ((v as u32) << 8); }
    pub fn set_room_id(&mut self, v: u8) { self.data1 = (self.data1 & 0xFFE0_FFFF) | (((v & 0x1F) as u32) << 16); }
    pub fn set_ped_density(&mut self, v: u8) { self.data1 = (self.data1 & 0xFF1F_FFFF) | (((v & 0x7) as u32) << 21); }
    pub fn set_flags(&mut self, v: u16) {
        self.data1 = (self.data1 & 0x00FF_FFFF) | ((v as u32 & 0xFF) << 24);
        self.data2 = (self.data2 & 0xFFFF_FF00) | ((v as u32 & 0xFF00) >> 8);
    }
    pub fn set_material_colour_index(&mut self, v: u8) { self.data2 = (self.data2 & 0xFFFF_00FF) | ((v as u32) << 8); }
    pub fn set_unk4(&mut self, v: u16) { self.data2 = (self.data2 & 0x0000_FFFF) | ((v as u32) << 16); }

    /// `BoundMaterial_s.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut) {
        x.value("Type", self.type_());
        x.value("ProceduralID", self.procedural_id());
        x.value("RoomID", self.room_id());
        x.value("PedDensity", self.ped_density());
        x.string("Flags", &format_flags(self.flags(), &MATERIAL_FLAG_NAMES, |v, f| v & f == f, 0, "NONE"));
        x.value("MaterialColourIndex", self.material_colour_index());
        x.value("Unk", self.unk4());
    }
    /// `BoundMaterial_s.ReadXml`.
    pub fn read_xml(n: Node) -> BoundMaterial {
        let mut m = BoundMaterial::default();
        m.set_type(child_attr_u32(n, "Type", "value") as u8);
        m.set_procedural_id(child_attr_u32(n, "ProceduralID", "value") as u8);
        m.set_room_id(child_attr_u32(n, "RoomID", "value") as u8);
        m.set_ped_density(child_attr_u32(n, "PedDensity", "value") as u8);
        m.set_flags(parse_flags(&child_text(n, "Flags"), &MATERIAL_FLAG_NAMES, |a, b| a | b, 0));
        m.set_material_colour_index(child_attr_u32(n, "MaterialColourIndex", "value") as u8);
        m.set_unk4(child_attr_u32(n, "Unk", "value") as u16);
        m
    }
}
impl Pod for BoundMaterial {
    const SIZE: usize = 8;
    fn write(&self, w: &mut Writer) { w.u32(self.data1); w.u32(self.data2); }
    fn read(b: &[u8]) -> Self { BoundMaterial { data1: u32::read(b), data2: u32::read(&b[4..]) } }
}

/// `BoundMaterialColour`: r, g, b, a.
impl Pod for [u8; 4] {
    const SIZE: usize = 4;
    fn write(&self, w: &mut Writer) { w.bytes(self); }
    fn read(b: &[u8]) -> Self { [b[0], b[1], b[2], b[3]] }
}

/// `BoundVertex_s`: a vertex quantised to `i16`, in units of the geometry's `Quantum`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoundVertex(pub i16, pub i16, pub i16);

impl BoundVertex {
    /// `new BoundVertex_s(v / quantum)`: each axis clamped to +-32767 and truncated.
    pub fn from_vec(v: Vec3, quantum: Vec3) -> BoundVertex {
        // C# `Math.Min(Math.Max(NaN, ..), ..)` stays NaN and the cast gives 0.
        let q = |f: f32| if f.is_nan() { 0 } else { f.clamp(-32767.0, 32767.0) as i16 };
        let d = vdiv(v, quantum);
        BoundVertex(q(d.x), q(d.y), q(d.z))
    }
    /// `bv.Vector * Quantum`.
    pub fn to_vec(self, quantum: Vec3) -> Vec3 { vmul(Vec3::new(self.0 as f32, self.1 as f32, self.2 as f32), quantum) }
}
impl Pod for BoundVertex {
    const SIZE: usize = 6;
    fn write(&self, w: &mut Writer) { w.i16(self.0); w.i16(self.1); w.i16(self.2); }
    fn read(b: &[u8]) -> Self { BoundVertex(i16::read(b), i16::read(&b[2..]), i16::read(&b[4..])) }
}

// ─── polygons ───────────────────────────────────────────────────────────────

/// A `BoundPolygon` (16 bytes on disk). `material` is the index into the geometry's materials
/// (the `PolygonMaterialIndices` entry, or the `m` attribute of the XML); triangle vertex indices
/// carry the flag in bit 15 and edge indices are `i16` with -1 for none.
/// The unused bytes of the primitives (`sphereType`, `unused0/1` ...) are written as zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Polygon {
    Triangle { material: u8, area: f32, v: [u16; 3], edges: [i16; 3] },
    Sphere { material: u8, index: u16, radius: f32 },
    Capsule { material: u8, i1: u16, i2: u16, radius: f32 },
    Box { material: u8, i: [i16; 4] },
    Cylinder { material: u8, i1: u16, i2: u16, radius: f32 },
}

fn put_u16(out: &mut [u8; 16], at: usize, v: u16) { out[at..at + 2].copy_from_slice(&v.to_le_bytes()); }
fn put_f32(out: &mut [u8; 16], at: usize, v: f32) { out[at..at + 4].copy_from_slice(&v.to_le_bytes()); }
fn get_u16(b: &[u8; 16], at: usize) -> u16 { u16::from_le_bytes([b[at], b[at + 1]]) }
fn get_f32(b: &[u8; 16], at: usize) -> f32 { f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) }

impl Polygon {
    /// `BoundPolygonType` as stored in the low 3 bits of byte 0.
    pub fn kind_byte(&self) -> u8 {
        match self { Polygon::Triangle { .. } => 0, Polygon::Sphere { .. } => 1, Polygon::Capsule { .. } => 2, Polygon::Box { .. } => 3, Polygon::Cylinder { .. } => 4 }
    }
    /// `BoundPolygonType.ToString()`, the XML element name.
    pub fn name(&self) -> &'static str {
        match self { Polygon::Triangle { .. } => "Triangle", Polygon::Sphere { .. } => "Sphere", Polygon::Capsule { .. } => "Capsule", Polygon::Box { .. } => "Box", Polygon::Cylinder { .. } => "Cylinder" }
    }
    pub fn material(&self) -> u8 {
        match *self { Polygon::Triangle { material, .. } | Polygon::Sphere { material, .. } | Polygon::Capsule { material, .. } | Polygon::Box { material, .. } | Polygon::Cylinder { material, .. } => material }
    }
    pub fn set_material(&mut self, m: u8) {
        match self { Polygon::Triangle { material, .. } | Polygon::Sphere { material, .. } | Polygon::Capsule { material, .. } | Polygon::Box { material, .. } | Polygon::Cylinder { material, .. } => *material = m }
    }

    /// `BoundPolygon.Write`: the 16 bytes with the kind bits of byte 0 still clear.
    pub fn write(&self, out: &mut [u8; 16]) {
        *out = [0; 16];
        match *self {
            Polygon::Triangle { area, v, edges, .. } => {
                put_f32(out, 0, area);
                for i in 0..3 { put_u16(out, 4 + 2 * i, v[i]); put_u16(out, 10 + 2 * i, edges[i] as u16); }
            }
            Polygon::Sphere { index, radius, .. } => { put_u16(out, 2, index); put_f32(out, 4, radius); }
            Polygon::Capsule { i1, i2, radius, .. } | Polygon::Cylinder { i1, i2, radius, .. } => { put_u16(out, 2, i1); put_f32(out, 4, radius); put_u16(out, 8, i2); }
            Polygon::Box { i, .. } => { for (k, v) in i.iter().enumerate() { put_u16(out, 4 + 2 * k, *v as u16); } }
        }
    }
    /// `Write` plus what `GetReferences` does after it: the kind ORed into the low 3 bits of byte 0.
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        self.write(&mut b);
        b[0] = (b[0] & 0xF8) | (self.kind_byte() & 7);
        b
    }
    /// `ReadPolygons` + `BoundPolygon.Read`: the kind is byte 0 & 7, the byte is masked before decoding.
    /// `None` for a kind the reference tool has no polygon for. The material is left 0 for the caller to set.
    pub fn read(b: &[u8]) -> Option<Polygon> {
        let mut d = [0u8; 16];
        d.copy_from_slice(b.get(..16)?);
        let kind = d[0] & 7;
        d[0] &= 0xF8;
        Some(match kind {
            0 => Polygon::Triangle {
                material: 0, area: get_f32(&d, 0), v: [get_u16(&d, 4), get_u16(&d, 6), get_u16(&d, 8)],
                edges: [get_u16(&d, 10) as i16, get_u16(&d, 12) as i16, get_u16(&d, 14) as i16],
            },
            1 => Polygon::Sphere { material: 0, index: get_u16(&d, 2), radius: get_f32(&d, 4) },
            2 => Polygon::Capsule { material: 0, i1: get_u16(&d, 2), i2: get_u16(&d, 8), radius: get_f32(&d, 4) },
            3 => Polygon::Box { material: 0, i: [get_u16(&d, 4) as i16, get_u16(&d, 6) as i16, get_u16(&d, 8) as i16, get_u16(&d, 10) as i16] },
            4 => Polygon::Cylinder { material: 0, i1: get_u16(&d, 2), i2: get_u16(&d, 8), radius: get_f32(&d, 4) },
            _ => return None,
        })
    }

    /// `BoundPolygon*.WriteXml`; `material_index` is `Owner.GetMaterialIndex(Index)`.
    pub fn write_xml(&self, x: &mut XmlOut, material_index: u8) {
        let name = self.name();
        let body = match *self {
            Polygon::Triangle { v, .. } => format!(
                "{name} m=\"{material_index}\" v1=\"{}\" v2=\"{}\" v3=\"{}\" f1=\"{}\" f2=\"{}\" f3=\"{}\"",
                v[0] & 0x7FFF, v[1] & 0x7FFF, v[2] & 0x7FFF, v[0] >> 15, v[1] >> 15, v[2] >> 15,
            ),
            Polygon::Sphere { index, radius, .. } => format!("{name} m=\"{material_index}\" v=\"{index}\" radius=\"{}\"", float(radius)),
            Polygon::Capsule { i1, i2, radius, .. } | Polygon::Cylinder { i1, i2, radius, .. } => {
                format!("{name} m=\"{material_index}\" v1=\"{i1}\" v2=\"{i2}\" radius=\"{}\"", float(radius))
            }
            Polygon::Box { i, .. } => format!("{name} m=\"{material_index}\" v1=\"{}\" v2=\"{}\" v3=\"{}\" v4=\"{}\"", i[0], i[1], i[2], i[3]),
        };
        x.self_closing(&body);
    }
    /// `BoundPolygon*.ReadXml` for the element `n` (its name is the kind). The material is the `m` attribute
    /// (an index into the XML's `Materials`); triangle areas and edges are 0 until they are recomputed.
    pub fn read_xml(n: Node) -> Result<Polygon> {
        let material = attr_i32(n, "m").clamp(0, 255) as u8;
        let u16_attr = |a: &str| attr_u32(n, a) as u16;
        let i16_attr = |a: &str| attr_i32(n, a) as i16;
        Ok(match n.tag_name().name() {
            "Triangle" => {
                let v = |i: &str, f: &str| (attr_i32(n, i) as u16 & 0x7FFF) | if attr_i32(n, f) != 0 { 0x8000 } else { 0 };
                Polygon::Triangle { material, area: 0.0, v: [v("v1", "f1"), v("v2", "f2"), v("v3", "f3")], edges: [0; 3] }
            }
            "Sphere" => Polygon::Sphere { material, index: u16_attr("v"), radius: super::xml::attr_f32(n, "radius") },
            "Capsule" => Polygon::Capsule { material, i1: u16_attr("v1"), i2: u16_attr("v2"), radius: super::xml::attr_f32(n, "radius") },
            "Box" => Polygon::Box { material, i: [i16_attr("v1"), i16_attr("v2"), i16_attr("v3"), i16_attr("v4")] },
            "Cylinder" => Polygon::Cylinder { material, i1: u16_attr("v1"), i2: u16_attr("v2"), radius: super::xml::attr_f32(n, "radius") },
            other => bail!("unknown bound polygon kind {other:?}"),
        })
    }

    /// `BoxMin`/`BoxMax`; `verts` are the vertex positions (`GetVertexPos`) indexed like the polygon's indices.
    pub fn bbox(&self, verts: &[Vec3]) -> (Vec3, Vec3) {
        let p = |i: usize| verts.get(i).copied().unwrap_or(Vec3::ZERO);
        let r = |v: Vec3, f: f32| (vadd_f(v, -f), vadd_f(v, f));
        match *self {
            Polygon::Triangle { v, .. } => {
                let (a, b, c) = (p((v[0] & 0x7FFF) as usize), p((v[1] & 0x7FFF) as usize), p((v[2] & 0x7FFF) as usize));
                (vmin(vmin(a, b), c), vmax(vmax(a, b), c))
            }
            Polygon::Sphere { index, radius, .. } => r(p(index as usize), radius),
            Polygon::Capsule { i1, i2, radius, .. } | Polygon::Cylinder { i1, i2, radius, .. } => {
                let (a, b) = (p(i1 as usize), p(i2 as usize));
                (vadd_f(vmin(a, b), -radius), vadd_f(vmax(a, b), radius))
            }
            Polygon::Box { i, .. } => {
                let q = |k: usize| p(i[k] as u16 as usize);
                (vmin(vmin(vmin(q(0), q(1)), q(2)), q(3)), vmax(vmax(vmax(q(0), q(1)), q(2)), q(3)))
            }
        }
    }

    fn triangle(&self) -> Option<([usize; 3], [i32; 3])> {
        match *self {
            Polygon::Triangle { v, edges, .. } => Some((v.map(|i| (i & 0x7FFF) as usize), edges.map(unpack_edge))),
            _ => None,
        }
    }
}

/// `BoundPolygonTriangle.UnpackEdgeIndex`.
fn unpack_edge(e: i16) -> i32 { if e as u16 == 0xFFFF { -1 } else { e as u16 as i32 } }
/// `BoundPolygonTriangle.PackEdgeIndex`.
fn pack_edge(p: i32) -> i16 { if !(0..=0xFFFF).contains(&p) { -1 } else { p as u16 as i16 } }

// ─── octants ────────────────────────────────────────────────────────────────

/// `BoundGeomOctants`: eight vertex-index lists; 128 bytes plus 4 per item. Written as the 8 counts,
/// 8 pointers to the lists (which follow), the lists and 32 zero bytes.
#[derive(Clone, Default, Debug)]
pub struct Octants { pub items: [Vec<u32>; 8] }

impl Octants {
    /// Reads the counts at `va` and the lists through the pointer array at `items_ptr`.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, items_ptr: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let counts: [u32; 8] = std::array::from_fn(|_| c.u32());
        c.check()?;
        if items_ptr == 0 { bail!("octants at {va:#x} have no item pointers"); }
        let ptrs = r.u64s(items_ptr, 8)?;
        let mut items: [Vec<u32>; 8] = Default::default();
        for i in 0..8 { items[i] = r.u32s(ptrs[i], counts[i] as usize)?; }
        let id = g.add(Octants { items });
        r.cache::<Self>(va, id);
        Ok(Some(id))
    }
}
impl Block for Octants {
    fn length(&self) -> usize { 128 + 4 * self.items.iter().map(Vec::len).sum::<usize>() }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> {
        let mut ptr = w.position() + 96;
        for l in &self.items { w.u32(l.len() as u32); }
        for l in &self.items { w.u64(ptr); ptr += 4 * l.len() as u64; }
        for l in &self.items { for &v in l { w.u32(v); } }
        w.zeros(32);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

// ─── the common 112 bytes ───────────────────────────────────────────────────

/// `Bounds` (112 bytes): the fields every bound shares. `transform` and the composite flags are only used
/// (written to XML, read from XML) for a child of a composite; `composite_flags1`/`2` are the `Flags1`/`Flags2`
/// of the `CompositeFlags1` struct, which `Bounds.WriteXml` writes as the `CompositeFlags1`/`CompositeFlags2` tags.
#[derive(Clone, Debug)]
pub struct BoundCommon {
    pub kind: BoundKind, pub sphere_radius: f32, pub box_max: Vec3, pub margin: f32, pub box_min: Vec3, pub unknown_3ch: u32,
    pub box_center: Vec3, pub material_index: u8, pub procedural_id: u8, pub room_id: u8, pub ped_density: u8, pub unk_flags: u8,
    pub sphere_center: Vec3, pub poly_flags: u8, pub material_color_index: u8, pub inertia: Vec3, pub volume: f32,
    pub transform: Mat4, pub composite_flags1: u32, pub composite_flags2: u32,
    /// `FileVFT`: a file's own, else the class's.
    pub vft: u32,
    /// The `FilePagesInfo` block of a root bound; a child of a composite has none.
    pub pages: Option<BlockId>,
}

impl BoundCommon {
    /// A bound of `kind` with the reference tool's defaults (`Unknown_3Ch = 1`, an identity transform, the class's `FileVFT`).
    pub fn new(kind: BoundKind) -> Self {
        let z = Vec3::ZERO;
        BoundCommon {
            kind, sphere_radius: 0.0, box_max: z, margin: 0.0, box_min: z, unknown_3ch: 1, box_center: z, material_index: 0, procedural_id: 0,
            room_id: 0, ped_density: 0, unk_flags: 0, sphere_center: z, poly_flags: 0, material_color_index: 0, inertia: z, volume: 0.0,
            transform: Mat4::identity(), composite_flags1: 0, composite_flags2: 0, vft: kind.default_vft(), pages: None,
        }
    }

    /// `Bounds.Write` (after `ResourceFileBase.Write`).
    fn write(&self, w: &mut Writer, g: &Graph) {
        write_file_base(w, g, self.vft, self.pages);
        w.u8(self.kind as u8); w.u8(0); w.u16(0);
        w.f32(self.sphere_radius); w.u32(0); w.u32(0);
        w.vec3(self.box_max); w.f32(self.margin);
        w.vec3(self.box_min); w.u32(self.unknown_3ch);
        w.vec3(self.box_center);
        w.u8(self.material_index); w.u8(self.procedural_id); w.u8((self.room_id & 0x1F) | ((self.ped_density & 7) << 5)); w.u8(self.unk_flags);
        w.vec3(self.sphere_center);
        w.u8(self.poly_flags); w.u8(self.material_color_index); w.u16(0);
        w.vec3(self.inertia); w.f32(self.volume);
    }

    /// `Bounds.Read` up to the pages pointer, which the caller resolves.
    fn read(c: &mut Cursor, kind: BoundKind, vft: u32) -> BoundCommon {
        let mut b = BoundCommon::new(kind);
        b.vft = vft;
        c.skip(4); // type, Unknown_11h, Unknown_12h
        b.sphere_radius = c.f32(); c.skip(8);
        b.box_max = c.vec3(); b.margin = c.f32();
        b.box_min = c.vec3(); b.unknown_3ch = c.u32();
        b.box_center = c.vec3();
        b.material_index = c.u8(); b.procedural_id = c.u8();
        let rp = c.u8(); b.room_id = rp & 0x1F; b.ped_density = rp >> 5;
        b.unk_flags = c.u8();
        b.sphere_center = c.vec3();
        b.poly_flags = c.u8(); b.material_color_index = c.u8(); c.skip(2);
        b.inertia = c.vec3(); b.volume = c.f32();
        b
    }

    /// `Bounds.WriteXml`; `parent` is the composite the bound is a child of, if any.
    fn write_xml(&self, x: &mut XmlOut, parent: Option<&CompositeCtx>) {
        x.vec3("BoxMin", self.box_min);
        x.vec3("BoxMax", self.box_max);
        x.vec3("BoxCenter", self.box_center);
        x.vec3("SphereCenter", self.sphere_center);
        x.value("SphereRadius", float(self.sphere_radius));
        x.value("Margin", float(self.margin));
        x.value("Volume", float(self.volume));
        x.vec3("Inertia", self.inertia);
        x.value("MaterialIndex", self.material_index);
        x.value("MaterialColourIndex", self.material_color_index);
        x.value("ProceduralID", self.procedural_id);
        x.value("RoomID", self.room_id & 0x1F);
        x.value("PedDensity", self.ped_density & 7);
        x.value("UnkFlags", self.unk_flags);
        x.value("PolyFlags", self.poly_flags);
        x.value("UnkType", self.unknown_3ch);
        if let Some(p) = parent {
            x.raw_array("CompositeTransform", &self.transform.0, 4, |f| float(*f));
            if !p.owner_is_fragment {
                let name = |f: u32| format_flags(f, &COMPOSITE_FLAG_NAMES, |v, b| v & b == b, 0, "NONE");
                x.string("CompositeFlags1", &name(self.composite_flags1));
                x.string("CompositeFlags2", &name(self.composite_flags2));
            }
        }
    }

    /// `Bounds.ReadXml`.
    fn read_xml(&mut self, n: Node, parent: Option<&CompositeCtx>) -> Result<()> {
        self.box_min = child_vec3(n, "BoxMin");
        self.box_max = child_vec3(n, "BoxMax");
        self.box_center = child_vec3(n, "BoxCenter");
        self.sphere_center = child_vec3(n, "SphereCenter");
        self.sphere_radius = child_attr_f32(n, "SphereRadius", "value");
        self.margin = child_attr_f32(n, "Margin", "value");
        self.volume = child_attr_f32(n, "Volume", "value");
        self.inertia = child_vec3(n, "Inertia");
        self.material_index = child_attr_u32(n, "MaterialIndex", "value") as u8;
        self.material_color_index = child_attr_u32(n, "MaterialColourIndex", "value") as u8;
        self.procedural_id = child_attr_u32(n, "ProceduralID", "value") as u8;
        self.room_id = child_attr_u32(n, "RoomID", "value") as u8 & 0x1F;
        self.ped_density = child_attr_u32(n, "PedDensity", "value") as u8 & 7;
        self.unk_flags = child_attr_u32(n, "UnkFlags", "value") as u8;
        self.poly_flags = child_attr_u32(n, "PolyFlags", "value") as u8;
        self.unknown_3ch = child_attr_u32(n, "UnkType", "value") as u8 as u32;
        if let Some(p) = parent {
            let t = child(n, "CompositeTransform").map(raw_f32s).unwrap_or_default();
            let Ok(rows) = <[f32; 16]>::try_from(t.as_slice()) else { bail!("CompositeTransform needs 16 numbers, found {}", t.len()) };
            self.transform = Mat4::from_d3d(rows);
            if !p.owner_is_fragment {
                let flags = |name: &str| parse_flags(&child_text(n, name), &COMPOSITE_FLAG_NAMES, |a, b| a | b, 0);
                self.composite_flags1 = flags("CompositeFlags1");
                self.composite_flags2 = flags("CompositeFlags2");
            }
        }
        Ok(())
    }
}

// ─── geometry ───────────────────────────────────────────────────────────────

/// The fixed fields of a geometry as stored: pointers and counts to resolve once the cursor is done.
struct GeoFields {
    shrunk_ptr: u64, unknown_82h: u16, shrunk_count: u32, polygons_ptr: u64, quantum: Vec3, unknown_9ch: f32, center_geom: Vec3, unknown_ach: f32,
    vertices_ptr: u64, vertex_colours_ptr: u64, octants_ptr: u64, octant_items_ptr: u64, vertices_count: u32, polygons_count: u32,
    materials_ptr: u64, material_colours_ptr: u64, polygon_materials_ptr: u64, materials_count: u8, material_colours_count: u8,
}

/// `BoundGeometry` (304 bytes) and, for [`BoundBlock::GeometryBvh`], `BoundBVH` (336 bytes; `bvh` is not ported yet).
/// The arrays are plain values; [`Geometry::prepare`] derives what a file needs and creates the blocks that
/// hold the arrays when the bound is saved. Empty arrays stand for the reference tool's null ones.
#[derive(Clone, Debug)]
pub struct Geometry {
    pub common: BoundCommon,
    pub quantum: Vec3, pub unknown_9ch: f32, pub center_geom: Vec3, pub unknown_ach: f32,
    pub vertices: Vec<Vec3>, pub vertices_shrunk: Option<Vec<Vec3>>, pub polygons: Vec<Polygon>,
    pub materials: Vec<BoundMaterial>, pub material_colours: Vec<[u8; 4]>, pub vertex_colours: Vec<[u8; 4]>, pub polygon_material_indices: Vec<u8>,
    pub octants: Option<BlockId>,
    /// The `BVH` of a [`BoundBlock::GeometryBvh`]; `prepare` builds it.
    pub bvh: Option<BlockId>,
    pub unknown_82h: u16,
    shrunk_block: Option<BlockId>, polygons_block: Option<BlockId>, vertices_block: Option<BlockId>, vertex_colours_block: Option<BlockId>,
    materials_block: Option<BlockId>, material_colours_block: Option<BlockId>, polygon_materials_block: Option<BlockId>,
    /// Set for a geometry read from a file: `prepare` then keeps its shrunk vertices and octants, as `GetReferences` does.
    keep_derived: bool,
    /// Set by `prepare`; [`BoundBlock::prepare_tree`] leaves a prepared geometry alone.
    prepared: bool,
}

impl Geometry {
    pub fn new(common: BoundCommon) -> Geometry {
        Geometry {
            common, quantum: Vec3::ZERO, unknown_9ch: 0.0, center_geom: Vec3::ZERO, unknown_ach: 0.0, vertices: Vec::new(), vertices_shrunk: None,
            polygons: Vec::new(), materials: Vec::new(), material_colours: Vec::new(), vertex_colours: Vec::new(), polygon_material_indices: Vec::new(),
            octants: None, bvh: None, unknown_82h: 0, shrunk_block: None, polygons_block: None, vertices_block: None, vertex_colours_block: None,
            materials_block: None, material_colours_block: None, polygon_materials_block: None, keep_derived: false, prepared: false,
        }
    }

    /// `GetVertexPos`: the vertex plus `CenterGeom`, through the bound's transform.
    fn vertex_pos(&self, i: usize) -> Vec3 {
        let v = self.vertices.get(i).copied().unwrap_or(Vec3::ZERO) + self.center_geom;
        self.common.transform.transform_point(v).xyz()
    }

    /// `CalculateQuantum`.
    pub fn calculate_quantum(&mut self) {
        let e = self.common.box_max - self.common.box_min;
        let h = e * 0.5;
        self.quantum = Vec3::new(h.x / 32767.0, h.y / 32767.0, h.z / 32767.0);
    }

    /// `BuildMaterials`: the polygons' materials, deduplicated in order of first use; every polygon then indexes them.
    pub fn build_materials(&mut self) {
        let mut seen: HashMap<BoundMaterial, u8> = HashMap::new();
        let mut list: Vec<BoundMaterial> = Vec::new();
        let mut indices = Vec::with_capacity(self.polygons.len());
        for p in &self.polygons {
            let m = self.materials.get(p.material() as usize).copied().unwrap_or_default();
            let idx = *seen.entry(m).or_insert_with(|| { list.push(m); (list.len() - 1) as u8 });
            indices.push(idx);
        }
        for (p, &i) in self.polygons.iter_mut().zip(&indices) { p.set_material(i); }
        self.materials = list;
        self.polygon_material_indices = indices;
    }

    /// `UpdateEdgeIndices`: each triangle edge points at the other triangle that shares it, -1 when none.
    pub fn update_edge_indices(&mut self) {
        struct Edge { t1: usize, id1: usize, t2: Option<(usize, usize)> }
        let mut map: HashMap<(i32, i32), usize> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        for i in 0..self.polygons.len() {
            let Some((v, _)) = self.polygons[i].triangle() else { continue };
            let v = v.map(|x| x as i32);
            for slot in 0..3 {
                let (a, b) = (v[slot], v[(slot + 1) % 3]);
                let key = (a.min(b), a.max(b));
                match map.get(&key) {
                    Some(&e) => {
                        if edges[e].t2.is_some() {
                            let t1 = edges[e].t1 as i32;
                            self.set_edge(i, slot, t1);
                        } else {
                            edges[e].t2 = Some((i, slot));
                        }
                    }
                    None => { map.insert(key, edges.len()); edges.push(Edge { t1: i, id1: slot, t2: None }); }
                }
            }
        }
        for e in &edges {
            match e.t2 {
                None => self.set_edge(e.t1, e.id1, -1),
                Some((t2, id2)) => {
                    self.set_edge(e.t1, e.id1, t2 as i32);
                    self.set_edge(t2, id2, e.t1 as i32);
                }
            }
        }
    }
    fn set_edge(&mut self, poly: usize, slot: usize, target: i32) {
        if let Polygon::Triangle { edges, .. } = &mut self.polygons[poly] { edges[slot] = pack_edge(target); }
    }

    /// `UpdateTriangleAreas`.
    pub fn update_triangle_areas(&mut self) {
        for i in 0..self.polygons.len() {
            let Some((v, _)) = self.polygons[i].triangle() else { continue };
            let a = triangle_area(self.vertex_pos(v[0]), self.vertex_pos(v[1]), self.vertex_pos(v[2]));
            if let Polygon::Triangle { area, .. } = &mut self.polygons[i] { *area = a; }
        }
    }

    /// `CalculateVertsShrunk`: only for a plain `Geometry` (a BVH has none). Shrinks by the margin along the
    /// (weighted) vertex normals, halving the margin until no shrunk vertex crosses a polygon, then falls back
    /// to shrinking by the plain vertex normals. The result is kept inside the box.
    pub fn calculate_verts_shrunk(&mut self) {
        self.vertices_shrunk = None;
        if self.common.kind != BoundKind::Geometry || self.vertices.is_empty() { return; }
        let size = (self.common.box_max - self.common.box_min).abs() * 0.5;
        let mut margin = self.common.margin.min(size.x).min(size.y).min(size.z);
        while margin > 1e-6 {
            let verts = self.shrink_polys_by_margin(margin);
            if self.check_shrunk_polys(&verts) {
                self.vertices_shrunk = Some(verts);
                break;
            }
            margin *= 0.5;
        }
        let Some(shrunk) = &mut self.vertices_shrunk else {
            self.calculate_verts_shrunk_by_normals();
            return;
        };
        let margin = margin.max(0.025);
        let shrunk_min = vadd_f(self.common.box_min, margin) - self.center_geom;
        let shrunk_max = vadd_f(self.common.box_max, -margin) - self.center_geom;
        for v in shrunk.iter_mut() { *v = vmax(vmin(*v, shrunk_max), shrunk_min); }
    }

    /// `ShrinkPolysByMargin`: each vertex moves against its normal, found by walking the ring of triangles around it.
    fn shrink_polys_by_margin(&self, margin: f32) -> Vec<Vec3> {
        let mut verts = self.vertices.clone();
        let vc = verts.len();
        let mut poly_normals = vec![Vec3::ZERO; self.polygons.len()];
        for (i, p) in self.polygons.iter().enumerate() {
            let Some((v, _)) = p.triangle() else { continue };
            if v.iter().any(|&i| i >= vc) { continue; }
            let (v1, v2, v3) = (self.vertices[v[0]], self.vertices[v[1]], self.vertices[v[2]]);
            poly_normals[i] = norm((v3 - v2).cross(v1 - v2));
        }

        let mut normals = [Vec3::ZERO; 64]; // the polygon's own normal, then its neighbours' (at most 63)
        let mut processed = vec![0u32; 2048];
        let neg = -margin;
        for poly_index in 0..self.polygons.len() {
            let Some((tv, tedges)) = self.polygons[poly_index].triangle() else { continue };
            for pvi in 0..3 {
                let vertex_index = tv[pvi];
                if vertex_index >= vc { continue; }
                let (bucket, mask) = (vertex_index >> 5, 1u32 << (vertex_index & 0x1F));
                if processed[bucket] & mask != 0 { continue; }
                processed[bucket] |= mask;

                let vertex = verts[vertex_index];
                let normal = poly_normals[poly_index];
                normals[0] = normal;
                let mut average = normal;
                let mut prev_neighbour = poly_index as i32;
                let mut normal_count = 1usize;
                let mut neighbour_count = 0usize;
                let mut neighbour = tedges[(pvi + 2) % 3];
                if neighbour < 0 { neighbour = tedges[pvi]; }
                while neighbour >= 0 {
                    let nb = neighbour as usize;
                    let Some(np) = self.polygons.get(nb) else { break };
                    let neighbour_normal = poly_normals[nb];
                    average = average + neighbour_normal;
                    normals[neighbour_count + 1] = neighbour_normal;
                    normal_count += 1;
                    neighbour_count += 1;
                    let mut new_neighbour = -1;
                    if let Some((nv, ne)) = np.triangle() {
                        for j in 0..3 {
                            let next = (j + 1) % 3;
                            if nv[next] == vertex_index {
                                new_neighbour = ne[j];
                                if new_neighbour == prev_neighbour { new_neighbour = ne[next]; }
                                prev_neighbour = neighbour;
                                neighbour = new_neighbour;
                                break;
                            }
                        }
                    }
                    if new_neighbour == poly_index as i32 { break; } // the ring is closed
                    if neighbour_count >= 63 { break; } // too many neighbours, possibly a mesh error
                }
                average = norm(average);

                if normal_count == 1 {
                    verts[vertex_index] = vertex + normal * neg;
                } else if normal_count == 2 {
                    let cross = normal.cross(normals[1]);
                    let mag_sq = cross.dot(cross);
                    if mag_sq < 0.1 { // a small angle between the normals: shrink by the polygon's own
                        verts[vertex_index] = vertex + normal * neg;
                        continue;
                    }
                    normals[2] = cross * (1.0 / mag_sq.sqrt());
                    normal_count = 3;
                }
                if normal_count < 3 { continue; }

                let nn = normal_count as i32 - 1;
                let mut shrunk = vertex + average * neg;
                for i in 0..nn - 1 {
                    for j in 0..nn - i - 1 {
                        for k in 0..nn - j - i - 1 {
                            let (n1, n2, n3) = (normals[i as usize], normals[(i + j + 1) as usize], normals[(i + j + k + 2) as usize]);
                            let cross23 = n2.cross(n3);
                            let dot = n1.dot(cross23);
                            if dot.abs() > 0.25 {
                                let new_normal = (cross23 + n3.cross(n1) + n1.cross(n2)) * (1.0 / dot);
                                let new_shrink = vertex + new_normal * neg;
                                let (to_old, to_new) = (shrunk - vertex, new_shrink - vertex);
                                if to_new.dot(to_new) > to_old.dot(to_old) { shrunk = new_shrink; }
                            }
                        }
                    }
                }
                verts[vertex_index] = shrunk;
            }
        }
        verts
    }

    /// `CheckShrunkPolys`: no vertex's shrink segment may cross a triangle that does not use the vertex.
    fn check_shrunk_polys(&self, verts: &[Vec3]) -> bool {
        let vc = verts.len();
        if vc != self.vertices.len() { return false; }
        for i in 0..vc {
            let vertex = self.vertices[i];
            let shrunk = verts[i];
            let mut dir = vertex - shrunk;
            let length = dir.length();
            if length == 0.0 { continue; } // not shrunk
            dir = dir * (1.0 / length);
            for p in &self.polygons {
                let Some((v, _)) = p.triangle() else { continue };
                if v.iter().any(|&x| x >= vc) { return false; }
                if v.contains(&i) { continue; }
                let hits = |a: Vec3, b: Vec3, c: Vec3| ray_triangle(shrunk, dir, a, b, c).is_some_and(|d| d <= length);
                if hits(self.vertices[v[0]], self.vertices[v[1]], self.vertices[v[2]]) { return false; }
                if hits(verts[v[0]], verts[v[1]], verts[v[2]]) { return false; }
            }
        }
        true
    }

    /// `CalculateVertsShrunkByNormals`: every vertex moves `Margin` against its normal.
    pub fn calculate_verts_shrunk_by_normals(&mut self) {
        if self.common.kind != BoundKind::Geometry { self.vertices_shrunk = None; return; }
        let normals = self.calculate_vert_normals();
        let m = -self.common.margin;
        self.vertices_shrunk = Some(self.vertices.iter().zip(&normals).map(|(&v, &n)| v + n * m).collect());
    }

    /// `CalculateVertNormals`: the sum of the normals of the triangles using each vertex, normalised.
    pub fn calculate_vert_normals(&self) -> Vec<Vec3> {
        let mut normals = vec![Vec3::ZERO; self.vertices.len()];
        for p in &self.polygons {
            let Some((v, _)) = p.triangle() else { continue };
            if v.iter().any(|&i| i >= normals.len()) { continue; }
            let (p1, p2, p3) = (self.vertex_pos(v[0]), self.vertex_pos(v[1]), self.vertex_pos(v[2]));
            let n = norm((p1 - p2).cross(p3 - p2));
            for i in v { normals[i] = normals[i] + n; }
        }
        for n in &mut normals {
            if *n == Vec3::ZERO { continue; }
            *n = norm(*n);
        }
        normals
    }

    /// `CalculateOctants` (a plain `Geometry` only; a BVH has none, so this is empty for it): for each of the
    /// eight sign octants, the shrunk vertices no other shrunk vertex shadows.
    pub fn calculate_octants(&self) -> Octants {
        let mut out = Octants::default();
        if self.common.kind != BoundKind::Geometry { return out; }
        let shrunk: &[Vec3] = self.vertices_shrunk.as_deref().unwrap_or(&[]);
        for (oct, list) in out.items.iter_mut().enumerate() {
            let flip = Vec3::new(if oct & 1 != 0 { -1.0 } else { 1.0 }, if oct & 2 != 0 { -1.0 } else { 1.0 }, if oct & 4 != 0 { -1.0 } else { 1.0 });
            let shadowed = |v1: Vec3, v2: Vec3| { let d = vmul(v2 - v1, flip); d.x >= 0.0 && d.y >= 0.0 && d.z >= 0.0 };
            let mut kept: Vec<u32> = Vec::new();
            for (i1, &v1) in shrunk.iter().enumerate() {
                if kept.iter().any(|&i2| shadowed(v1, shrunk[i2 as usize])) { continue; }
                kept.retain(|&i2| !shadowed(shrunk[i2 as usize], v1));
                kept.push(i1 as u32);
            }
            *list = kept;
        }
        out
    }

    /// What `ReadXml` and `GetReferences` do to a geometry before it is written: `BuildMaterials`,
    /// `CalculateQuantum`, `UpdateEdgeIndices`, `UpdateTriangleAreas`, then (unless the geometry was read from
    /// a file, whose shrunk vertices and octants are kept) `CalculateVertsShrunk` and `CalculateOctants`,
    /// and finally the blocks holding the arrays: shrunk vertices, polygon bytes, vertices, vertex colours,
    /// octants, materials (padded to four), material colours and polygon material indices.
    pub fn prepare(&mut self, g: &mut Graph) {
        self.build_materials();
        self.calculate_quantum();
        self.update_edge_indices();
        self.update_triangle_areas();
        if !self.keep_derived {
            self.calculate_verts_shrunk();
            self.octants = (self.common.kind == BoundKind::Geometry).then(|| { let o = self.calculate_octants(); g.add(o) });
        }
        if self.common.kind == BoundKind::GeometryBvh {
            // `BoundBVH.GetReferences`: `BuildBVH(false)`, then the base's steps again on the reordered polygons
            self.rebuild_bvh(g);
            self.build_materials();
            self.calculate_quantum();
            self.update_edge_indices();
            self.update_triangle_areas();
        }

        // The file holds the vertices on the (recomputed) quantum grid; so does the graph from here on, so
        // that its XML is what the written file reads back as. A file's own quantum need not be
        // `CalculateQuantum`'s, and `new BoundVertex_s(v / Quantum)` truncates, so a vertex can move by up
        // to one quantum, as it does in the reference tool.
        let q = self.quantum;
        let quantise = |vs: &mut Vec<Vec3>| {
            let items: Vec<BoundVertex> = vs.iter().map(|&v| BoundVertex::from_vec(v, q)).collect();
            *vs = items.iter().map(|bv| bv.to_vec(q)).collect();
            StructArray { items }
        };
        self.shrunk_block = self.vertices_shrunk.as_mut().map(|v| g.add(quantise(v)));
        self.polygons_block = (!self.polygons.is_empty()).then(|| {
            let data = self.polygons.iter().flat_map(|p| p.to_bytes()).collect();
            g.add(RawBytes { data, section: Section::System })
        });
        self.vertices_block = (!self.vertices.is_empty()).then(|| g.add(quantise(&mut self.vertices)));
        self.vertex_colours_block = (!self.vertex_colours.is_empty()).then(|| g.add(StructArray { items: self.vertex_colours.clone() }));
        let mut mats = self.materials.clone();
        if mats.len() < 4 { mats.resize(4, BoundMaterial::default()); }
        self.materials_block = Some(g.add(StructArray { items: mats }));
        self.material_colours_block = (!self.material_colours.is_empty()).then(|| g.add(StructArray { items: self.material_colours.clone() }));
        self.polygon_materials_block = (!self.polygon_material_indices.is_empty()).then(|| g.add(StructArray { items: self.polygon_material_indices.clone() }));
        self.prepared = true;
    }

    /// `BoundBVH.BuildBVH(false)` (`Bounds.cs:2602`): a BVH over the polygons' boxes (item threshold 4); the
    /// polygons and their material indices are reordered to node order, triangle edge indices follow them
    /// through the lookup, and the bound's box and sphere become the BVH's box. No BVH for no polygons.
    fn rebuild_bvh(&mut self, g: &mut Graph) {
        if self.polygons.is_empty() { self.bvh = None; return; }
        let verts: Vec<Vec3> = (0..self.vertices.len()).map(|i| self.vertex_pos(i)).collect();
        let items: Vec<Option<BvhItem>> = self.polygons.iter().enumerate().map(|(i, p)| { let (min, max) = p.bbox(&verts); Some(BvhItem { min, max, index: i }) }).collect();
        let built = build_bvh(g, &items, 4);

        let mut lookup = vec![0usize; self.polygons.len()]; // old index -> new index
        for (new, &old) in built.item_order.iter().enumerate() { lookup[old] = new; }
        let mut polygons = Vec::with_capacity(self.polygons.len());
        let mut materials = Vec::with_capacity(self.polygons.len());
        for &old in &built.item_order {
            let mut p = self.polygons[old];
            materials.push(self.polygon_material_indices.get(old).copied().unwrap_or(0));
            if let Polygon::Triangle { edges, .. } = &mut p {
                // `edgeIndex` is a ushort: 0xFFFF and anything past the polygons is no edge
                *edges = edges.map(|e| pack_edge(lookup.get(e as u16 as usize).map_or(-1, |&n| n as i32)));
            }
            polygons.push(p);
        }
        self.polygons = polygons;
        self.polygon_material_indices = materials;

        let bvh = &built.bvh;
        let c = &mut self.common;
        c.box_min = bvh.bb_min.xyz(); c.box_max = bvh.bb_max.xyz(); c.box_center = bvh.bb_center.xyz();
        c.sphere_center = c.box_center;
        c.sphere_radius = (c.box_max - c.box_center).length();
        self.bvh = Some(g.add(built.bvh));
    }

    /// `BoundGeometry.WriteXml` (after `Bounds.WriteXml`).
    fn write_xml(&self, x: &mut XmlOut) {
        x.vec3("GeometryCenter", self.center_geom);
        x.value("UnkFloat1", float(self.unknown_9ch));
        x.value("UnkFloat2", float(self.unknown_ach));
        write_items(x, "Materials", &self.materials, |x, m| m.write_xml(x));
        let colour = |c: &[u8; 4]| format!("{}, {}, {}, {}", c[0], c[1], c[2], c[3]);
        if !self.material_colours.is_empty() { x.raw_array("MaterialColours", &self.material_colours, 1, colour); }
        if !self.vertices.is_empty() { x.raw_array("Vertices", &self.vertices, 1, |v| format!("{}, {}, {}", float(v.x), float(v.y), float(v.z))); }
        if !self.vertex_colours.is_empty() { x.raw_array("VertexColours", &self.vertex_colours, 1, colour); }
        if !self.polygons.is_empty() {
            x.open("Polygons");
            for (i, p) in self.polygons.iter().enumerate() { p.write_xml(x, self.polygon_material_indices.get(i).copied().unwrap_or(0)); }
            x.close("Polygons");
        }
    }

    /// `BoundGeometry.ReadXml` (after `Bounds.ReadXml`): the arrays, then the derived data via [`Geometry::prepare`].
    fn read_xml(&mut self, n: Node, g: &mut Graph) -> Result<()> {
        self.center_geom = child_vec3(n, "GeometryCenter");
        self.unknown_9ch = child_attr_f32(n, "UnkFloat1", "value");
        self.unknown_ach = child_attr_f32(n, "UnkFloat2", "value");
        self.materials = items(n, "Materials").into_iter().map(BoundMaterial::read_xml).collect();
        self.material_colours = child(n, "MaterialColours").map(raw_rgba).unwrap_or_default();
        self.vertices = child(n, "Vertices").map(raw_vec3s).unwrap_or_default();
        self.vertex_colours = child(n, "VertexColours").map(raw_rgba).unwrap_or_default();
        if let Some(p) = child(n, "Polygons") {
            for e in p.children().filter(|c| c.is_element()) { self.polygons.push(Polygon::read_xml(e)?); }
        }
        if self.common.kind == BoundKind::GeometryBvh {
            // `BoundBVH.ReadXml` stops after the base's steps; `BuildBVH` runs in `GetReferences`, once the composite
            // above has built its own BVH from the children's boxes as written in the XML (see `BoundBlock::prepare_tree`).
            self.build_materials();
            self.calculate_quantum();
            self.update_edge_indices();
            self.update_triangle_areas();
        } else {
            self.prepare(g);
        }
        Ok(())
    }

    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        if !self.polygons.is_empty() && self.polygons_block.is_none() { bail!("a geometry bound must be prepared before it is written"); }
        self.common.write(w, g);
        let vertices = if self.vertices_block.is_some() { self.vertices.len() as u32 } else { 0 };
        let octants = g.ptr(self.octants);
        w.u32(0); w.u32(0);
        w.u64(g.ptr(self.shrunk_block));
        w.u16(0); w.u16(self.unknown_82h);
        w.u32(vertices);
        w.u64(g.ptr(self.polygons_block));
        w.vec3(self.quantum); w.f32(self.unknown_9ch);
        w.vec3(self.center_geom); w.f32(self.unknown_ach);
        w.u64(g.ptr(self.vertices_block)); w.u64(g.ptr(self.vertex_colours_block));
        w.u64(octants); w.u64(if octants != 0 { octants + 32 } else { 0 });
        w.u32(vertices); w.u32(self.polygons.len() as u32);
        w.zeros(24);
        w.u64(g.ptr(self.materials_block)); w.u64(g.ptr(self.material_colours_block));
        w.zeros(24);
        w.u64(g.ptr(self.polygon_materials_block));
        w.u8(count_u8(self.materials.len(), "materials in a geometry bound")?);
        w.u8(if self.material_colours_block.is_some() { count_u8(self.material_colours.len(), "material colours in a geometry bound")? } else { 0 });
        w.u16(0); w.zeros(12);
        Ok(())
    }

    fn references(&self) -> Vec<BlockId> {
        [self.common.pages, self.shrunk_block, self.polygons_block, self.vertices_block, self.vertex_colours_block, self.octants,
            self.materials_block, self.material_colours_block, self.polygon_materials_block, self.bvh].into_iter().flatten().collect()
    }

    /// The fixed fields of `BoundGeometry.Read` after the common ones.
    fn read_fields(c: &mut Cursor) -> GeoFields {
        c.skip(8);
        let shrunk_ptr = c.u64();
        c.skip(2);
        let unknown_82h = c.u16();
        let shrunk_count = c.u32();
        let polygons_ptr = c.u64();
        let quantum = c.vec3(); let unknown_9ch = c.f32();
        let center_geom = c.vec3(); let unknown_ach = c.f32();
        let vertices_ptr = c.u64(); let vertex_colours_ptr = c.u64();
        let octants_ptr = c.u64(); let octant_items_ptr = c.u64();
        let vertices_count = c.u32(); let polygons_count = c.u32();
        c.skip(24);
        let materials_ptr = c.u64(); let material_colours_ptr = c.u64();
        c.skip(24);
        let polygon_materials_ptr = c.u64();
        let materials_count = c.u8(); let material_colours_count = c.u8();
        c.skip(2 + 12);
        GeoFields { shrunk_ptr, unknown_82h, shrunk_count, polygons_ptr, quantum, unknown_9ch, center_geom, unknown_ach, vertices_ptr, vertex_colours_ptr, octants_ptr, octant_items_ptr, vertices_count, polygons_count, materials_ptr, material_colours_ptr, polygon_materials_ptr, materials_count, material_colours_count }
    }

    /// The arrays of `BoundGeometry.Read`; they become values (the blocks are made again by `prepare`).
    fn read(r: &mut Reader, g: &mut Graph, f: GeoFields, common: BoundCommon) -> Result<Geometry> {
        let GeoFields { shrunk_ptr, unknown_82h, shrunk_count, polygons_ptr, quantum, unknown_9ch, center_geom, unknown_ach, vertices_ptr, vertex_colours_ptr, octants_ptr, octant_items_ptr, vertices_count, polygons_count, materials_ptr, material_colours_ptr, polygon_materials_ptr, materials_count, material_colours_count } = f;

        let dequantise = |vs: Vec<BoundVertex>| vs.into_iter().map(|v| v.to_vec(quantum)).collect::<Vec<_>>();
        let shrunk = dequantise(r.structs::<BoundVertex>(shrunk_ptr, shrunk_count as usize)?);
        let vertices_shrunk = if shrunk.is_empty() { None } else { Some(shrunk) };

        let polygon_material_indices = r.bytes(polygon_materials_ptr, polygons_count as usize)?;
        let mut polygons = Vec::new();
        if polygons_count != 0 {
            let Some(len) = (polygons_count as usize).checked_mul(16) else { bail!("{polygons_count} polygons overflow") };
            let data = r.bytes(polygons_ptr, len)?;
            if data.len() != len { bail!("the polygons pointer is null but there are {polygons_count} polygons"); }
            for (i, b) in data.chunks_exact(16).enumerate() {
                let Some(mut p) = Polygon::read(b) else { bail!("polygon {i} has unknown kind {}", b[0] & 7) };
                p.set_material(polygon_material_indices.get(i).copied().unwrap_or(0));
                polygons.push(p);
            }
        }
        let vertices = dequantise(r.structs::<BoundVertex>(vertices_ptr, vertices_count as usize)?);
        let vertex_colours = r.structs::<[u8; 4]>(vertex_colours_ptr, vertices_count as usize)?;
        let octants = Octants::read(r, g, octants_ptr, octant_items_ptr)?;
        // The reference tool reads at least four material slots and drops the padding again.
        let materials = r.structs::<BoundMaterial>(materials_ptr, materials_count as usize)?;
        let material_colours = r.structs::<[u8; 4]>(material_colours_ptr, material_colours_count as usize)?;

        let mut geo = Geometry::new(common);
        geo.quantum = quantum; geo.unknown_9ch = unknown_9ch; geo.center_geom = center_geom; geo.unknown_ach = unknown_ach;
        geo.vertices = vertices; geo.vertices_shrunk = vertices_shrunk; geo.polygons = polygons; geo.materials = materials;
        geo.material_colours = material_colours; geo.vertex_colours = vertex_colours; geo.polygon_material_indices = polygon_material_indices;
        geo.octants = octants; geo.unknown_82h = unknown_82h; geo.keep_derived = true;
        Ok(geo)
    }
}

// ─── composite ──────────────────────────────────────────────────────────────

/// The fixed fields of a composite as stored: pointers and the child count, resolved once the cursor is done.
struct CompositeFields { children_ptr: u64, transform1_ptr: u64, transform2_ptr: u64, flags1_ptr: u64, count: usize, bvh_ptr: u64 }

/// `BoundComposite` (176 bytes). `children` are bound blocks (`None` for a null child); each child holds its own
/// `transform` and composite flags. The arrays derived from them (transforms, boxes, flags, the BVH) are made by
/// [`Composite::prepare`]. `ChildrenFlags2` is written as a copy of `ChildrenFlags1`, as `ReadXml` builds it.
#[derive(Clone, Debug)]
pub struct Composite {
    pub common: BoundCommon,
    pub children: Vec<Option<BlockId>>,
    pub bvh: Option<BlockId>,
    /// `OwnerIsFragment`: a fragment's composite has no child flags and marks its transforms with `0x7f800001`.
    pub owner_is_fragment: bool,
    children_block: Option<BlockId>, transforms_block: Option<BlockId>, bboxes_block: Option<BlockId>,
    flags1_block: Option<BlockId>, flags2_block: Option<BlockId>,
    /// Set by `prepare`; [`BoundBlock::prepare_tree`] leaves a prepared composite alone.
    prepared: bool,
}

impl Composite {
    pub fn new(common: BoundCommon) -> Composite {
        Composite {
            common, children: Vec::new(), bvh: None, owner_is_fragment: false, children_block: None, transforms_block: None,
            bboxes_block: None, flags1_block: None, flags2_block: None, prepared: false,
        }
    }

    fn ctx(&self) -> CompositeCtx { CompositeCtx { owner_is_fragment: self.owner_is_fragment } }

    /// What `BoundComposite.ReadXml` and `GetReferences` do before the composite is written: `BuildBVH`,
    /// `UpdateChildrenFlags`, `UpdateChildrenBounds`, `UpdateChildrenTransformations`, then the blocks holding the arrays.
    /// The children are read as they are now; a `GeometryBvh` child updates its own box only when it is prepared.
    pub fn prepare(&mut self, g: &mut Graph) {
        self.build_bvh(g);
        let child = |g: &Graph, id: Option<BlockId>| id.map(|id| g.get::<BoundBlock>(id).common().clone());
        let kids: Vec<Option<BoundCommon>> = self.children.iter().map(|&id| child(g, id)).collect();

        // UpdateChildrenFlags: none in a fragment (the child's Flags1/Flags2 are `CompositeFlags1`, twice)
        let flags: Vec<ChildFlags> = kids.iter().map(|k| k.as_ref().map_or(ChildFlags(0, 0), |c| ChildFlags(c.composite_flags1, c.composite_flags2))).collect();
        let with_flags = !self.owner_is_fragment && !kids.is_empty();
        self.flags1_block = with_flags.then(|| g.add(StructArray { items: flags.clone() }));
        self.flags2_block = with_flags.then(|| g.add(StructArray { items: flags }));

        // UpdateChildrenBounds: `float.Epsilon` is the smallest denormal, not `f32::EPSILON`
        let boxes: Vec<Aabb> = kids.iter().map(|k| k.as_ref().map_or(Aabb { min: Vec4::new(0.0, 0.0, 0.0, 0.0), max: Vec4::new(0.0, 0.0, 0.0, 0.0) }, |c| Aabb {
            min: Vec4::new(c.box_min.x, c.box_min.y, c.box_min.z, f32::from_bits(1)), max: Vec4::new(c.box_max.x, c.box_max.y, c.box_max.z, c.margin),
        })).collect();
        self.bboxes_block = (!boxes.is_empty()).then(|| g.add(StructArray { items: boxes }));

        // UpdateChildrenTransformations: `ChildrenTransformation2` is null and shares the first array's pointer
        let marks = if self.owner_is_fragment { [0x7f80_0001; 4] } else { [0, 1, 1, 0] };
        let transforms: Vec<ChildTransform> = kids.iter().map(|k| ChildTransform { m: k.as_ref().map_or(Mat4::identity(), |c| c.transform), flags: marks }).collect();
        self.transforms_block = (!transforms.is_empty()).then(|| g.add(StructArray { items: transforms }));

        self.children_block = (!self.children.is_empty()).then(|| g.add(PointerArray64 { items: self.children.clone() }));
        self.prepared = true;
    }

    /// `BuildBVH` (`Bounds.cs:3114`): a BVH with item threshold 1, only for more than five children; the items
    /// are the children's boxes through their transforms, and null children count towards the capacity.
    fn build_bvh(&mut self, g: &mut Graph) {
        self.bvh = None;
        if self.children.len() <= 5 { return; }
        let items: Vec<Option<BvhItem>> = self.children.iter().enumerate().map(|(index, id)| id.map(|id| {
            let c = g.get::<BoundBlock>(id).common();
            let (min, max) = transform_box(c.box_min, c.box_max, &c.transform);
            BvhItem { min, max, index }
        })).collect();
        let built = build_bvh(g, &items, 1);
        self.bvh = Some(g.add(built.bvh));
    }

    /// `BoundComposite.WriteXml` after `Bounds.WriteXml`.
    fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        if self.children.is_empty() { x.self_closing("Children"); return; }
        let ctx = self.ctx();
        x.open("Children");
        for child in &self.children {
            match child {
                Some(id) => g.get::<BoundBlock>(*id).write_xml(x, g, Some(&ctx)),
                None => BoundBlock::write_xml_none(x, "Item"),
            }
        }
        x.close("Children");
    }

    /// `BoundComposite.ReadXml` after `Bounds.ReadXml`: the children (each read under this composite), then `prepare`.
    fn read_xml(&mut self, n: Node, g: &mut Graph) -> Result<()> {
        let ctx = self.ctx();
        for item in items(n, "Children") { self.children.push(BoundBlock::read_xml(item, g, Some(&ctx))?); }
        self.prepare(g);
        Ok(())
    }

    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        if !self.children.is_empty() && self.children_block.is_none() { bail!("a composite bound must be prepared before it is written"); }
        self.common.write(w, g);
        let transforms = g.ptr(self.transforms_block);
        w.u64(g.ptr(self.children_block)); w.u64(transforms); w.u64(transforms); // ChildrenTransformation2Pointer falls back to the first
        w.u64(g.ptr(self.bboxes_block)); w.u64(g.ptr(self.flags1_block)); w.u64(g.ptr(self.flags2_block));
        let n = count_u16(self.children.len(), "children in a composite bound")?;
        w.u16(n); w.u16(n); w.u32(0);
        w.u64(g.ptr(self.bvh));
        Ok(())
    }

    fn references(&self) -> Vec<BlockId> {
        [self.common.pages, self.children_block, self.transforms_block, self.bboxes_block, self.flags1_block, self.flags2_block, self.bvh].into_iter().flatten().collect()
    }

    fn read_fields(c: &mut Cursor) -> CompositeFields {
        let (children_ptr, transform1_ptr, transform2_ptr) = (c.u64(), c.u64(), c.u64());
        c.skip(8); // the children's boxes are derived again
        let flags1_ptr = c.u64();
        c.skip(8); // ChildrenFlags2: written as a copy of the first
        let count = c.u16() as usize;
        c.skip(2 + 4);
        CompositeFields { children_ptr, transform1_ptr, transform2_ptr, flags1_ptr, count, bvh_ptr: c.u64() }
    }

    /// `BoundComposite.Read`: the children are read under this composite, each given its transform (from
    /// `ChildrenTransformation1`, else `2`) and its flags.
    fn read(r: &mut Reader, g: &mut Graph, f: CompositeFields, common: BoundCommon) -> Result<Composite> {
        let ctx = CompositeCtx { owner_is_fragment: false };
        let mut comp = Composite::new(common);
        let pointers = r.u64s(f.children_ptr, f.count)?;
        let transforms = match r.structs::<ChildTransform>(f.transform1_ptr, f.count)? { t if t.is_empty() => r.structs::<ChildTransform>(f.transform2_ptr, f.count)?, t => t };
        let flags1 = r.structs::<ChildFlags>(f.flags1_ptr, f.count)?;
        for (i, &ptr) in pointers.iter().enumerate() {
            let child = BoundBlock::read(r, g, ptr, Some(&ctx))?;
            if let Some(id) = child {
                let c = g.get_mut::<BoundBlock>(id).common_mut();
                c.transform = transforms.get(i).map_or(Mat4::identity(), |t| t.m);
                let fl = flags1.get(i).copied().unwrap_or(ChildFlags(0, 0));
                c.composite_flags1 = fl.0;
                c.composite_flags2 = fl.1;
            }
            comp.children.push(child);
        }
        comp.bvh = if f.bvh_ptr != 0 { Bvh::read(r, g, f.bvh_ptr)? } else { None };
        Ok(comp)
    }
}

// ─── the block ──────────────────────────────────────────────────────────────

/// A bound of one of the ported kinds. `Cloth` is `BoundCloth`, whose `Read`/`Write` are the
/// 112-byte base and nothing else (its extra fields are commented out in the reference tool).
#[derive(Clone)]
pub enum BoundBlock {
    Sphere(BoundCommon), Capsule(BoundCommon), Box(BoundCommon), Disc(BoundCommon), Cylinder(BoundCommon),
    Geometry(Geometry), GeometryBvh(Geometry), Cloth(BoundCommon), Composite(Composite),
}

impl BoundBlock {
    pub fn common(&self) -> &BoundCommon {
        match self {
            BoundBlock::Sphere(c) | BoundBlock::Capsule(c) | BoundBlock::Box(c) | BoundBlock::Disc(c) | BoundBlock::Cylinder(c) | BoundBlock::Cloth(c) => c,
            BoundBlock::Geometry(g) | BoundBlock::GeometryBvh(g) => &g.common,
            BoundBlock::Composite(c) => &c.common,
        }
    }
    pub fn common_mut(&mut self) -> &mut BoundCommon {
        match self {
            BoundBlock::Sphere(c) | BoundBlock::Capsule(c) | BoundBlock::Box(c) | BoundBlock::Disc(c) | BoundBlock::Cylinder(c) | BoundBlock::Cloth(c) => c,
            BoundBlock::Geometry(g) | BoundBlock::GeometryBvh(g) => &mut g.common,
            BoundBlock::Composite(c) => &mut c.common,
        }
    }
    /// Gives a root bound its `PagesInfo`; a child of a composite has none.
    pub fn set_pages(&mut self, pages: Option<BlockId>) { self.common_mut().pages = pages; }

    /// `Bounds.WriteXmlNode` of a null bound.
    pub fn write_xml_none(x: &mut XmlOut, name: &str) { x.self_closing(&format!("{name} type=\"None\"")); }

    /// `Bounds.WriteXmlNode`: `<Bounds type="..">` for a root bound, `<Item type="..">` inside a composite.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, in_composite: Option<&CompositeCtx>) {
        let name = if in_composite.is_some() { "Item" } else { "Bounds" };
        let c = self.common();
        x.open_attrs(name, &format!("type=\"{}\"", c.kind.name()));
        c.write_xml(x, in_composite);
        match self {
            BoundBlock::Geometry(geo) | BoundBlock::GeometryBvh(geo) => geo.write_xml(x),
            BoundBlock::Composite(comp) => comp.write_xml(x, g),
            _ => {}
        }
        x.close(name);
    }

    /// `Bounds.ReadXmlNode`: `None` for `type="None"`. A geometry is prepared (its derived data computed and its
    /// array blocks added to `g`). An unknown or missing `type` is an error, where the reference tool would make a sphere.
    pub fn read_xml(n: Node, g: &mut Graph, parent: Option<&CompositeCtx>) -> Result<Option<BlockId>> {
        let ty = attr_str(n, "type");
        if ty == "None" { return Ok(None); }
        let Some(kind) = BoundKind::parse(ty) else { bail!("unknown bound type {ty:?}") };
        let mut common = BoundCommon::new(kind);
        common.read_xml(n, parent)?;
        let block = match kind {
            BoundKind::Composite => {
                let mut comp = Composite::new(common);
                comp.read_xml(n, g)?;
                BoundBlock::Composite(comp)
            }
            BoundKind::Sphere => BoundBlock::Sphere(common),
            BoundKind::Capsule => BoundBlock::Capsule(common),
            BoundKind::Box => BoundBlock::Box(common),
            BoundKind::Disc => BoundBlock::Disc(common),
            BoundKind::Cylinder => BoundBlock::Cylinder(common),
            BoundKind::Cloth => BoundBlock::Cloth(common),
            BoundKind::Geometry | BoundKind::GeometryBvh => {
                let mut geo = Geometry::new(common);
                geo.read_xml(n, g)?;
                if kind == BoundKind::Geometry { BoundBlock::Geometry(geo) } else { BoundBlock::GeometryBvh(geo) }
            }
        };
        Ok(Some(g.add(block)))
    }

    /// `Bounds.GetType` + `Read`: the kind is the byte at 0x10. A composite parent sets a child's transform
    /// and flags itself; a composite inside a composite is refused (none exists, and a cycle would never end).
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, parent: Option<&CompositeCtx>) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let pages_ptr = c.u64();
        let kind_byte = c.u8();
        c.check()?;
        let Some(kind) = BoundKind::from_byte(kind_byte) else { bail!("unknown bound type {kind_byte} at {va:#x}") };
        if kind == BoundKind::Composite && parent.is_some() { bail!("the composite bound at {va:#x} is inside another composite"); }
        // the common fields start at the type byte, 16 bytes in
        let (common, fields, bvh_ptr, composite) = {
            let mut c = r.cursor(va + 16)?;
            let common = BoundCommon::read(&mut c, kind, vft);
            let fields = match kind {
                BoundKind::Geometry | BoundKind::GeometryBvh => Some(Geometry::read_fields(&mut c)),
                BoundKind::Capsule | BoundKind::Disc | BoundKind::Cylinder => { c.skip(16); None }
                _ => None,
            };
            // BoundBVH: the BVH pointer, then Unknown_138h.. (written back as constants)
            let bvh_ptr = if kind == BoundKind::GeometryBvh { let p = c.u64(); c.skip(24); p } else { 0 };
            let composite = (kind == BoundKind::Composite).then(|| Composite::read_fields(&mut c));
            c.check()?;
            (common, fields, bvh_ptr, composite)
        };
        let mut common = common;
        common.pages = read_pages_info(r, g, pages_ptr)?;
        let block = match (kind, fields) {
            (BoundKind::Sphere, _) => BoundBlock::Sphere(common),
            (BoundKind::Box, _) => BoundBlock::Box(common),
            (BoundKind::Cloth, _) => BoundBlock::Cloth(common),
            (BoundKind::Capsule, _) => BoundBlock::Capsule(common),
            (BoundKind::Disc, _) => BoundBlock::Disc(common),
            (BoundKind::Cylinder, _) => BoundBlock::Cylinder(common),
            (BoundKind::Geometry, Some(f)) => BoundBlock::Geometry(Geometry::read(r, g, f, common)?),
            (BoundKind::GeometryBvh, Some(f)) => {
                let mut geo = Geometry::read(r, g, f, common)?;
                // `BvhPointer > 65535`: a smaller value is junk in some drawables' bounds
                if bvh_ptr > 65535 { geo.bvh = Bvh::read(r, g, bvh_ptr)?; }
                BoundBlock::GeometryBvh(geo)
            }
            (BoundKind::Composite, _) => BoundBlock::Composite(Composite::read(r, g, composite.expect("read with the composite fields"), common)?),
            _ => unreachable!("kind {kind:?} was checked above"),
        };
        let id = g.add(block);
        r.cache::<Self>(va, id);
        Ok(Some(id))
    }

    /// The `GetReferences` of a bound and everything under it, run before the graph is laid out: a geometry or
    /// composite that is not yet prepared (one read from a file) derives its arrays and blocks. A composite goes
    /// first, as in the reference tool, so its BVH and child boxes see the children as they were read; a bound prepared
    /// when its XML was read is left alone.
    pub fn prepare_tree(g: &mut Graph, id: BlockId) {
        let mut block = g.get::<BoundBlock>(id).clone();
        match &mut block {
            BoundBlock::Composite(comp) => { if !comp.prepared { comp.prepare(g); } }
            BoundBlock::Geometry(geo) | BoundBlock::GeometryBvh(geo) => { if !geo.prepared { geo.prepare(g); } }
            _ => {}
        }
        let children = if let BoundBlock::Composite(comp) = &block { comp.children.clone() } else { Vec::new() };
        *g.get_mut::<BoundBlock>(id) = block;
        for child in children.into_iter().flatten() { BoundBlock::prepare_tree(g, child); }
    }
}

impl Block for BoundBlock {
    fn length(&self) -> usize {
        match self {
            BoundBlock::Sphere(_) | BoundBlock::Box(_) | BoundBlock::Cloth(_) => 112,
            BoundBlock::Capsule(_) | BoundBlock::Disc(_) | BoundBlock::Cylinder(_) => 128,
            BoundBlock::Geometry(_) => 304,
            BoundBlock::GeometryBvh(_) => 336,
            BoundBlock::Composite(_) => 176,
        }
    }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        match self {
            BoundBlock::Geometry(geo) | BoundBlock::GeometryBvh(geo) => geo.references(),
            BoundBlock::Composite(comp) => comp.references(),
            other => other.common().pages.into_iter().collect(),
        }
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        match self {
            BoundBlock::Sphere(c) | BoundBlock::Box(c) | BoundBlock::Cloth(c) => c.write(w, g),
            BoundBlock::Capsule(c) | BoundBlock::Disc(c) | BoundBlock::Cylinder(c) => { c.write(w, g); w.zeros(16); }
            BoundBlock::Geometry(geo) => geo.write(w, g)?,
            BoundBlock::GeometryBvh(geo) => {
                geo.write(w, g)?;
                // BoundBVH: the BVH pointer, Unknown_138h/13Ch, Unknown_140h (0xFFFF), Unknown_142h..14Ch
                w.u64(g.ptr(geo.bvh)); w.u32(0); w.u32(0); w.u16(0xFFFF); w.u16(0); w.u32(0); w.u32(0); w.u32(0);
            }
            BoundBlock::Composite(comp) => comp.write(w, g)?,
        }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::PagesInfo;
    use crate::blocks::bvh::{Bvh, BvhNode};
    use crate::resource::SYSTEM_BASE;

    /// Four vertices (+-1, +-1, 0), two triangles sharing the edge (0, 2), one material of type 1.
    fn quad_geometry() -> Geometry {
        let mut c = BoundCommon::new(BoundKind::Geometry);
        c.margin = 0.04;
        let vertices = vec![Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0)];
        c.box_min = vertices.iter().fold(vertices[0], |a, &b| vmin(a, b));
        c.box_max = vertices.iter().fold(vertices[0], |a, &b| vmax(a, b));
        let mut geo = Geometry::new(c);
        geo.vertices = vertices;
        let tri = |a, b, c| Polygon::Triangle { material: 0, area: 0.0, v: [a, b, c], edges: [0; 3] };
        geo.polygons = vec![tri(0, 1, 2), tri(0, 2, 3)];
        let mut m = BoundMaterial::default();
        m.set_type(1);
        geo.materials = vec![m];
        geo
    }

    #[test]
    fn materials_are_deduplicated_and_edges_shared() {
        let mut geo = quad_geometry();
        geo.build_materials(); geo.update_edge_indices();
        assert_eq!(geo.materials.len(), 1);
        assert_eq!(geo.polygon_material_indices, vec![0, 0]);
        let Polygon::Triangle { edges, .. } = geo.polygons[0] else { panic!() };
        // Triangle (0,1,2) has edges (0,1), (1,2), (2,0): the shared edge (0,2) is its third, so the brief's
        // `[-1, 1, -1]` cannot come out of `UpdateEdgeIndices`.
        assert_eq!(edges, [-1, -1, 1], "edge (0,2) is shared with triangle 1");
        let Polygon::Triangle { edges, .. } = geo.polygons[1] else { panic!() };
        assert_eq!(edges, [0, -1, -1]);
    }

    #[test]
    fn quantum_and_vertex_quantisation_round_trip() {
        let mut geo = quad_geometry(); geo.calculate_quantum();
        assert!((geo.quantum.x - 2.0 * 0.5 / 32767.0).abs() < 1e-9);
        let q = BoundVertex::from_vec(Vec3::new(1.0, -1.0, 0.0), geo.quantum);
        assert_eq!((q.0, q.1), (32767, -32767));
    }

    #[test]
    fn shrunk_vertices_move_inwards_and_octants_partition() {
        let mut geo = quad_geometry(); geo.calculate_quantum(); geo.update_edge_indices();
        geo.calculate_verts_shrunk();
        let s = geo.vertices_shrunk.as_ref().unwrap();
        assert_eq!(s.len(), 4);
        assert!(s.iter().zip(&geo.vertices).all(|(a, b)| (a.x.abs() < b.x.abs() + 1e-6) && (a.y.abs() < b.y.abs() + 1e-6)));
        let oct = geo.calculate_octants();
        assert!(oct.items.iter().map(Vec::len).sum::<usize>() > 0);
    }

    fn cube() -> Geometry {
        let mut c = BoundCommon::new(BoundKind::Geometry);
        c.margin = 0.1; c.box_min = Vec3::new(-1.0, -1.0, -1.0); c.box_max = Vec3::new(1.0, 1.0, 1.0);
        let mut geo = Geometry::new(c);
        geo.vertices = (0..8).map(|i| Vec3::new(if i & 1 != 0 { 1.0 } else { -1.0 }, if i & 2 != 0 { 1.0 } else { -1.0 }, if i & 4 != 0 { 1.0 } else { -1.0 })).collect();
        // outward, counter-clockwise: +z, -z, +x, -x, +y, -y
        let tris: [[u16; 3]; 12] = [[4, 5, 7], [4, 7, 6], [0, 2, 3], [0, 3, 1], [1, 3, 7], [1, 7, 5], [0, 4, 6], [0, 6, 2], [2, 6, 7], [2, 7, 3], [0, 1, 5], [0, 5, 4]];
        geo.polygons = tris.iter().map(|&v| Polygon::Triangle { material: 0, area: 0.0, v, edges: [0; 3] }).collect();
        geo.materials = vec![BoundMaterial::default()];
        geo
    }

    #[test]
    fn a_cube_shrinks_every_corner_along_the_diagonal_and_octants_keep_the_extreme_corners() {
        let mut geo = cube();
        geo.build_materials(); geo.calculate_quantum(); geo.update_edge_indices(); geo.update_triangle_areas();
        for p in &geo.polygons {
            let Polygon::Triangle { edges, area, .. } = *p else { panic!() };
            assert!(edges.iter().all(|&e| e >= 0), "a closed mesh has a neighbour across every edge");
            assert!((area - 2.0).abs() < 1e-4, "{area}");
        }
        geo.calculate_verts_shrunk();
        let s = geo.vertices_shrunk.clone().unwrap();
        for (v, s) in geo.vertices.iter().zip(&s) {
            let want = *v * 0.9;
            assert!((*s - want).length() < 1e-4, "{v:?} -> {s:?}");
        }
        let oct = geo.calculate_octants();
        assert_eq!(oct.items[0], vec![7]);
        assert_eq!(oct.items[7], vec![0]);
        assert_eq!(oct.items[1], vec![6]);
    }

    #[test]
    fn octants_drop_shadowed_vertices_in_index_order() {
        let mut geo = quad_geometry();
        geo.vertices_shrunk = Some(vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0), Vec3::new(-1.0, 2.0, 0.5)]);
        let oct = geo.calculate_octants();
        assert_eq!(oct.items[0], vec![1, 2]);
        assert_eq!(oct.items[7], vec![0, 2]);
    }

    #[test]
    fn geometry_bound_has_reference_lengths_and_round_trips() {
        let mut g = Graph::new(); let pages = g.add(PagesInfo::default());
        let mut geo = quad_geometry(); geo.prepare(&mut g);
        let b = g.add(BoundBlock::Geometry(geo));
        g.get_mut::<BoundBlock>(b).set_pages(Some(pages));
        assert_eq!(g.length(b), 304);
        let file = g.build(b, pages, 43).unwrap();
        let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
        let b2 = BoundBlock::read(&mut r, &mut g2, SYSTEM_BASE, None).unwrap().unwrap();
        let BoundBlock::Geometry(geo2) = g2.get::<BoundBlock>(b2) else { panic!() };
        assert_eq!(geo2.vertices.len(), 4); assert_eq!(geo2.polygons.len(), 2);
        let x1 = { let mut x = XmlOut::new(); g.get::<BoundBlock>(b).write_xml(&mut x, &g, None); x.out };
        let x2 = { let mut x = XmlOut::new(); g2.get::<BoundBlock>(b2).write_xml(&mut x, &g2, None); x.out };
        assert_eq!(x1, x2);
        let old = crate::parse_ybn(&file).unwrap();
        assert_eq!(old.triangles().len(), 2, "the existing ybn parser reads the built bound");
    }

    #[test]
    fn xml_reads_back_to_the_same_xml_and_rebuilds_the_same_file() {
        let mut g = Graph::new(); let pages = g.add(PagesInfo::default());
        let mut geo = quad_geometry();
        geo.polygons.push(Polygon::Sphere { material: 0, index: 2, radius: 0.5 });
        geo.polygons.push(Polygon::Capsule { material: 0, i1: 0, i2: 1, radius: 0.25 });
        geo.polygons.push(Polygon::Box { material: 0, i: [0, 1, 2, 3] });
        geo.polygons.push(Polygon::Cylinder { material: 0, i1: 1, i2: 2, radius: 0.75 });
        geo.vertex_colours = vec![[1, 2, 3, 4]; 4];
        geo.material_colours = vec![[5, 6, 7, 8]];
        geo.prepare(&mut g);
        let b = g.add(BoundBlock::Geometry(geo));
        g.get_mut::<BoundBlock>(b).set_pages(Some(pages));
        let xml = { let mut x = XmlOut::new(); g.get::<BoundBlock>(b).write_xml(&mut x, &g, None); x.out };
        assert!(xml.contains("<Bounds type=\"Geometry\">") && xml.contains("Triangle m=\"0\" v1=\"0\" v2=\"1\" v3=\"2\" f1=\"0\" f2=\"0\" f3=\"0\""), "{xml}");
        assert!(xml.contains("<Flags>NONE</Flags>") && xml.contains("Cylinder m=\"0\" v1=\"1\" v2=\"2\" radius=\"0.75\""), "{xml}");

        let mut g3 = Graph::new(); let pages3 = g3.add(PagesInfo::default());
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let b3 = BoundBlock::read_xml(doc.root_element(), &mut g3, None).unwrap().unwrap();
        g3.get_mut::<BoundBlock>(b3).set_pages(Some(pages3));
        let xml3 = { let mut x = XmlOut::new(); g3.get::<BoundBlock>(b3).write_xml(&mut x, &g3, None); x.out };
        assert_eq!(xml, xml3);
        assert_eq!(g.build(b, pages, 43).unwrap(), g3.build(b3, pages3, 43).unwrap());
    }

    #[test]
    fn primitives_and_cloth_have_their_lengths_and_read_back() {
        for (kind, len) in [(BoundKind::Sphere, 112), (BoundKind::Capsule, 128), (BoundKind::Box, 112), (BoundKind::Disc, 128), (BoundKind::Cylinder, 128), (BoundKind::Cloth, 112)] {
            let mut g = Graph::new(); let pages = g.add(PagesInfo::default());
            let mut c = BoundCommon::new(kind);
            c.sphere_radius = 2.5; c.box_max = Vec3::new(1.0, 2.0, 3.0); c.margin = 0.04; c.room_id = 5; c.ped_density = 3; c.inertia = Vec3::new(4.0, 5.0, 6.0);
            let block = match kind {
                BoundKind::Sphere => BoundBlock::Sphere(c), BoundKind::Capsule => BoundBlock::Capsule(c), BoundKind::Box => BoundBlock::Box(c),
                BoundKind::Disc => BoundBlock::Disc(c), BoundKind::Cylinder => BoundBlock::Cylinder(c), _ => BoundBlock::Cloth(c),
            };
            let b = g.add(block);
            g.get_mut::<BoundBlock>(b).set_pages(Some(pages));
            assert_eq!(g.length(b), len, "{kind:?}");
            let file = g.build(b, pages, 43).unwrap();
            let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
            let b2 = BoundBlock::read(&mut r, &mut g2, SYSTEM_BASE, None).unwrap().unwrap();
            let c2 = g2.get::<BoundBlock>(b2).common();
            assert_eq!((c2.kind, c2.sphere_radius, c2.room_id, c2.ped_density, c2.unknown_3ch, c2.inertia.z), (kind, 2.5, 5, 3, 1, 6.0));
            assert_eq!(c2.vft, g.get::<BoundBlock>(b).common().vft);
            assert!(c2.pages.is_some());
            let x1 = { let mut x = XmlOut::new(); g.get::<BoundBlock>(b).write_xml(&mut x, &g, None); x.out };
            let doc = roxmltree::Document::parse(&x1).unwrap();
            let mut g3 = Graph::new();
            let b3 = BoundBlock::read_xml(doc.root_element(), &mut g3, None).unwrap().unwrap();
            let x3 = { let mut x = XmlOut::new(); g3.get::<BoundBlock>(b3).write_xml(&mut x, &g3, None); x.out };
            assert_eq!(x1, x3, "{kind:?}");
        }
    }

    #[test]
    fn composite_children_carry_a_transform_and_flags_in_xml() {
        let mut g = Graph::new();
        let mut c = BoundCommon::new(BoundKind::Sphere);
        c.transform = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        c.composite_flags1 = (1 << 2) | (1 << 9);
        let b = g.add(BoundBlock::Sphere(c));
        let ctx = CompositeCtx { owner_is_fragment: false };
        let mut x = XmlOut::new();
        g.get::<BoundBlock>(b).write_xml(&mut x, &g, Some(&ctx));
        assert!(x.out.contains("<Item type=\"Sphere\">") && x.out.contains("<CompositeFlags1>MAP_DYNAMIC, PED</CompositeFlags1>") && x.out.contains("<CompositeFlags2>NONE</CompositeFlags2>"), "{}", x.out);
        let doc = roxmltree::Document::parse(&x.out).unwrap();
        let b2 = BoundBlock::read_xml(doc.root_element(), &mut g, Some(&ctx)).unwrap().unwrap();
        let c2 = g.get::<BoundBlock>(b2).common();
        assert_eq!((c2.transform, c2.composite_flags1, c2.composite_flags2), (Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)), 516, 0));
        let mut x2 = XmlOut::new();
        BoundBlock::write_xml_none(&mut x2, "Item");
        assert!(x2.out.ends_with("<Item type=\"None\" />\n"));
        let doc = roxmltree::Document::parse(&x2.out).unwrap();
        assert!(BoundBlock::read_xml(doc.root_element(), &mut g, None).unwrap().is_none());
    }

    #[test]
    fn polygons_pack_their_kind_and_read_it_back() {
        let tri = Polygon::Triangle { material: 0, area: 1.5, v: [1, 0x8002, 3], edges: [-1, 7, 0] };
        let b = tri.to_bytes();
        assert_eq!(b[0] & 7, 0);
        let Some(Polygon::Triangle { v, edges, .. }) = Polygon::read(&b) else { panic!() };
        assert_eq!((v, edges), ([1, 0x8002, 3], [-1, 7, 0]));
        for p in [Polygon::Sphere { material: 0, index: 9, radius: 1.5 }, Polygon::Capsule { material: 0, i1: 1, i2: 2, radius: 0.5 },
                  Polygon::Box { material: 0, i: [-1, 2, 3, 4] }, Polygon::Cylinder { material: 0, i1: 5, i2: 6, radius: 2.0 }] {
            let b = p.to_bytes();
            assert_eq!(b[0] & 7, p.kind_byte());
            assert_eq!(Polygon::read(&b), Some(p));
        }
        assert_eq!(Polygon::read(&[7; 16]), None);
        let (lo, hi) = Polygon::Capsule { material: 0, i1: 0, i2: 1, radius: 0.5 }.bbox(&[Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0)]);
        assert_eq!((lo, hi), (Vec3::new(-0.5, -0.5, -0.5), Vec3::new(2.5, 0.5, 0.5)));
    }

    #[test]
    fn material_bits_are_packed_as_reference_does() {
        let mut m = BoundMaterial::default();
        m.set_type(0x12); m.set_procedural_id(0x34); m.set_room_id(0x1F); m.set_ped_density(5); m.set_flags(0x8103); m.set_material_colour_index(9); m.set_unk4(0xABCD);
        assert_eq!((m.data1, m.data2), (0x03BF_3412, 0xABCD_0981));
        assert_eq!((m.type_(), m.procedural_id(), m.room_id(), m.ped_density(), m.flags(), m.material_colour_index(), m.unk4()), (0x12, 0x34, 0x1F, 5, 0x8103, 9, 0xABCD));
    }

    #[test]
    fn octants_block_writes_counts_pointers_items_and_padding() {
        let mut g = Graph::new(); let pages = g.add(PagesInfo::default());
        let mut oct = Octants::default();
        oct.items[0] = vec![3, 4]; oct.items[7] = vec![9];
        let o = g.add(oct);
        assert_eq!(g.length(o), 128 + 12);
        let file = g.build(o, pages, 43).unwrap();
        let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
        let o2 = Octants::read(&mut r, &mut g2, SYSTEM_BASE, SYSTEM_BASE + 32).unwrap().unwrap();
        assert_eq!(g2.get::<Octants>(o2).items[0], vec![3, 4]);
        assert_eq!(g2.get::<Octants>(o2).items[7], vec![9]);
        assert!(g2.get::<Octants>(o2).items[3].is_empty());
    }

    // ─── composites and the BVH geometry ────────────────────────────────────

    /// A composite of `n` unit boxes at distinct centres along x, `owner_is_fragment: false`.
    fn composite_of_boxes(g: &mut Graph, n: usize) -> Composite {
        let mut comp = Composite::new(BoundCommon::new(BoundKind::Composite));
        for i in 0..n {
            let mut c = BoundCommon::new(BoundKind::Box);
            let at = Vec3::new(i as f32 * 3.0, 0.0, 0.0);
            c.box_min = at - Vec3::new(0.5, 0.5, 0.5); c.box_max = at + Vec3::new(0.5, 0.5, 0.5); c.box_center = at; c.margin = 0.04;
            c.transform = Mat4::from_translation(Vec3::new(0.0, i as f32, 0.0));
            comp.children.push(Some(g.add(BoundBlock::Box(c))));
        }
        comp
    }

    #[test]
    fn composite_bvh_is_padded_and_only_built_for_six_or_more_children() {
        let mut g = Graph::new();
        let mut five = composite_of_boxes(&mut g, 5); five.prepare(&mut g); assert!(five.bvh.is_none());
        let mut six = composite_of_boxes(&mut g, 6); six.prepare(&mut g);
        let bvh = g.get::<Bvh>(six.bvh.unwrap());
        assert_eq!(bvh.nodes_capacity, 13);
        let nodes = g.get::<StructArray<BvhNode>>(bvh.nodes.unwrap());
        assert!(nodes.items[bvh.nodes_count as usize..].iter().all(|n| n.item_id == 1 && n.item_count == 0));
        assert_eq!(nodes.items.len(), 13);
    }

    #[test]
    fn composite_derived_arrays_follow_updatechildren() {
        let mut g = Graph::new();
        let mut comp = composite_of_boxes(&mut g, 2);
        comp.children.push(None);
        comp.prepare(&mut g);
        let ts = &g.get::<StructArray<ChildTransform>>(comp.transforms_block.unwrap()).items;
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[1].m.translation(), Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(ts[1].flags, [0, 1, 1, 0]);
        assert_eq!(ts[2].m, Mat4::identity(), "a null child is the identity");
        let bb = &g.get::<StructArray<Aabb>>(comp.bboxes_block.unwrap()).items;
        assert_eq!((bb[1].min.x, bb[1].min.w.to_bits(), bb[1].max.w), (2.5, 1, 0.04), "float.Epsilon is the smallest denormal");
        assert_eq!((bb[2].min, bb[2].max), (Vec4::new(0.0, 0.0, 0.0, 0.0), Vec4::new(0.0, 0.0, 0.0, 0.0)));
        assert_eq!(g.get::<StructArray<ChildFlags>>(comp.flags1_block.unwrap()).items.len(), 3);

        let mut frag = composite_of_boxes(&mut g, 1);
        frag.owner_is_fragment = true;
        frag.prepare(&mut g);
        assert!(frag.flags1_block.is_none() && frag.flags2_block.is_none());
        assert_eq!(g.get::<StructArray<ChildTransform>>(frag.transforms_block.unwrap()).items[0].flags, [0x7f800001; 4]);
    }

    /// A strip of `n` triangles along x over vertices (k, 0, 0) and (k, 1, 0), as a BVH geometry.
    fn strip(n: usize) -> Geometry {
        let mut c = BoundCommon::new(BoundKind::GeometryBvh);
        c.margin = 0.04;
        let mut geo = Geometry::new(c);
        for k in 0..=n / 2 { geo.vertices.push(Vec3::new(k as f32, 0.0, 0.0)); geo.vertices.push(Vec3::new(k as f32, 1.0, 0.0)); }
        for k in 0..n / 2 {
            let k = 2 * k as u16;
            geo.polygons.push(Polygon::Triangle { material: 0, area: 0.0, v: [k, k + 1, k + 2], edges: [0; 3] });
            geo.polygons.push(Polygon::Triangle { material: 0, area: 0.0, v: [k + 1, k + 3, k + 2], edges: [0; 3] });
        }
        geo.common.box_min = Vec3::ZERO; geo.common.box_max = Vec3::new((n / 2) as f32, 1.0, 0.0);
        geo.materials = vec![BoundMaterial::default()];
        geo
    }

    /// Every edge index other than -1 must name a triangle that has both vertices of that edge.
    fn assert_edges_are_shared(geo: &Geometry) {
        let tri = |i: usize| match geo.polygons[i] { Polygon::Triangle { v, edges, .. } => (v, edges), _ => panic!() };
        let mut linked = 0;
        for i in 0..geo.polygons.len() {
            let (v, edges) = tri(i);
            for s in 0..3 {
                if edges[s] == -1 { continue; }
                let (other, _) = tri(edges[s] as usize);
                assert!(other.contains(&v[s]) && other.contains(&v[(s + 1) % 3]), "triangle {i} edge {s} -> {}", edges[s]);
                linked += 1;
            }
        }
        assert!(linked > 0);
    }

    #[test]
    fn bvh_geometry_reorders_polygons_and_remaps_edges() {
        let mut g = Graph::new();
        let mut geo = strip(8);
        geo.build_materials(); geo.calculate_quantum(); geo.update_edge_indices();
        let before = geo.polygons.clone();
        assert_edges_are_shared(&geo);
        geo.rebuild_bvh(&mut g);
        assert_ne!(geo.polygons, before, "the polygons are regrouped by BVH node");
        assert_eq!(geo.polygons.len(), 8);
        assert_eq!(geo.polygons[0], before[4].with_edges_of(&geo.polygons[0]), "the upper leaf comes first");
        assert_edges_are_shared(&geo); // the remap alone, before `update_edge_indices` would recompute them
        assert_eq!(geo.polygon_material_indices.len(), 8);

        let mut geo = strip(8);
        geo.prepare(&mut g);
        assert_edges_are_shared(&geo);
        let bvh = g.get::<Bvh>(geo.bvh.unwrap());
        assert_eq!((geo.common.box_min, geo.common.box_max), (bvh.bb_min.xyz(), bvh.bb_max.xyz()));
        assert_eq!((geo.common.box_center, geo.common.sphere_center), (bvh.bb_center.xyz(), bvh.bb_center.xyz()));
        assert_eq!(geo.common.sphere_radius, (geo.common.box_max - geo.common.box_center).length());
        assert_eq!((bvh.nodes_count, bvh.trees_count), (3, 1));
    }

    #[test]
    fn a_composite_with_a_bvh_and_a_strip_geometry_read_back_and_rewrite_identically() {
        use crate::resource::SYSTEM_BASE;
        let mut g = Graph::new(); let pages = g.add(PagesInfo::default());
        let mut comp = composite_of_boxes(&mut g, 6);
        // four triangles over columns 0..2, centred so every vertex is exactly representable at 32767 quanta
        let mut geo = strip(4);
        geo.vertices.iter_mut().for_each(|v| *v = *v - Vec3::new(1.0, 0.5, 0.0));
        geo.center_geom = Vec3::new(1.0, 0.5, 0.0);
        let gid = g.add(BoundBlock::GeometryBvh(geo));
        comp.children.push(Some(gid));
        comp.children.push(None);
        comp.prepare(&mut g);
        let root = g.add(BoundBlock::Composite(comp));
        g.get_mut::<BoundBlock>(root).set_pages(Some(pages));
        BoundBlock::prepare_tree(&mut g, root);
        assert_eq!(g.length(root), 176);
        let file = g.build(root, pages, 43).unwrap();

        let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
        let root2 = BoundBlock::read(&mut r, &mut g2, SYSTEM_BASE, None).unwrap().unwrap();
        let BoundBlock::Composite(c2) = g2.get::<BoundBlock>(root2) else { panic!() };
        assert_eq!(c2.children.len(), 8);
        assert!(c2.children[7].is_none());
        let bvh = g2.get::<Bvh>(c2.bvh.expect("seven children get a BVH"));
        assert_eq!(bvh.nodes_capacity, 17, "2 * 8 + 1, the null child included");
        let BoundBlock::Box(b) = g2.get::<BoundBlock>(c2.children[3].unwrap()) else { panic!() };
        assert_eq!(b.transform.translation(), Vec3::new(0.0, 3.0, 0.0), "each child gets its transform back");
        let BoundBlock::GeometryBvh(geo2) = g2.get::<BoundBlock>(c2.children[6].unwrap()) else { panic!() };
        assert!(geo2.bvh.is_some() && geo2.polygons.len() == 4);
        BoundBlock::prepare_tree(&mut g2, root2);
        assert!(g2.build(root2, g2.get::<BoundBlock>(root2).common().pages.unwrap(), 43).unwrap() == file, "a read graph rewrites the same file");
    }

    impl Polygon {
        /// Test helper: `self` with the edges of `other` (a triangle), to compare vertex indices only.
        fn with_edges_of(self, other: &Polygon) -> Polygon {
            match (self, other) {
                (Polygon::Triangle { material, area, v, .. }, Polygon::Triangle { edges, .. }) => Polygon::Triangle { material, area, v, edges: *edges },
                _ => self,
            }
        }
    }
}
