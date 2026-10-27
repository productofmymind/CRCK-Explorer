//! Navmeshes (`.ynv`), read and write. A map cell's navmesh is a list of
//! convex polygons over quantised vertices, each polygon edge naming the
//! neighbour it joins (in this cell or an adjacent one), plus portals
//! (climb/drop links), points and a quadtree of sectors the game uses to
//! look polygons up by position.
//!
//! Byte layout ported from the reference library `Nav.cs` / `YnvFile.cs`; the
//! writer follows `YnvFile.BuildStructs` so the reference tool and the game read
//! what this produces.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::math::Vec3;
use crate::resource::{build_rsc7_paged, prepare_rsc7, u16_le, u32_le, u64_le, vec3_le, ResReader, SYSTEM_BASE};

/// "No neighbour" on an edge: the 14-bit all-ones area/poly id.
pub const ADJACENT_NONE: u32 = 0x3FFF;

/// `NavMeshFlags` content bits.
pub const CONTENT_POLYGONS: u32 = 1;
pub const CONTENT_PORTALS: u32 = 2;
pub const CONTENT_VEHICLE: u32 = 4;

/// One side of an edge record: which polygon lies across this edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavEdgeEnd {
    /// Area id of the neighbour's cell, or [`ADJACENT_NONE`].
    pub area_id: u32,
    /// Polygon index inside that cell, or [`ADJACENT_NONE`].
    pub poly: u32,
    pub unk2: u8,
    pub unk3: u16,
}

impl NavEdgeEnd {
    pub const NONE: NavEdgeEnd = NavEdgeEnd { area_id: ADJACENT_NONE, poly: ADJACENT_NONE, unk2: 0, unk3: 0 };

    pub fn neighbour(area_id: u32, poly: u32) -> Self {
        Self { area_id, poly, unk2: 0, unk3: 0 }
    }

    pub fn is_none(&self) -> bool {
        self.poly == ADJACENT_NONE
    }
}

/// The edge leaving vertex `i` of a polygon (towards vertex `i+1`). Retail
/// files carry the same neighbour in both halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavEdge {
    pub a: NavEdgeEnd,
    pub b: NavEdgeEnd,
}

impl NavEdge {
    pub const NONE: NavEdge = NavEdge { a: NavEdgeEnd::NONE, b: NavEdgeEnd::NONE };

    pub fn neighbour(area_id: u32, poly: u32) -> Self {
        let end = NavEdgeEnd::neighbour(area_id, poly);
        Self { a: end, b: end }
    }
}

/// A navmesh polygon: world-space vertices, one edge record per vertex, and
/// the three raw flag words (accessors below name the known bits).
#[derive(Debug, Clone, PartialEq)]
pub struct NavPoly {
    pub vertices: Vec<Vec3>,
    pub edges: Vec<NavEdge>,
    /// `PolyFlags0` (16 bits): avoid/footpath/underground/steep/water bits.
    pub flags0: u16,
    /// `PolyFlags1`: underground, path node, interior (bit 6), flat ground
    /// (bit 9), road, cell edge, train track, shallow water, footpath bits.
    pub flags1: u32,
    /// `PolyFlags2`: two bytes the reference tool calls UnkX/UnkY, then slope
    /// direction bits from bit 16.
    pub flags2: u32,
    /// Indices into the file's portal-link table, one per portal this
    /// polygon touches.
    pub portal_links: Vec<u16>,
}

impl NavPoly {
    pub fn new(vertices: Vec<Vec3>) -> Self {
        let edges = vec![NavEdge::NONE; vertices.len()];
        Self { vertices, edges, flags0: 0, flags1: 0, flags2: 0, portal_links: Vec::new() }
    }

    pub fn is_interior(&self) -> bool { self.flags1 & 64 != 0 }
    pub fn set_interior(&mut self, on: bool) { set_bit(&mut self.flags1, 64, on) }
    pub fn is_flat_ground(&self) -> bool { self.flags1 & 512 != 0 }
    pub fn set_flat_ground(&mut self, on: bool) { set_bit(&mut self.flags1, 512, on) }
    pub fn is_road(&self) -> bool { self.flags1 & 1024 != 0 }
    pub fn is_cell_edge(&self) -> bool { self.flags1 & 2048 != 0 }
    pub fn is_footpath(&self) -> bool { self.flags0 & 4 != 0 }
    pub fn is_water(&self) -> bool { self.flags0 & 128 != 0 }
    pub fn is_steep(&self) -> bool { self.flags0 & 64 != 0 }

    pub fn centroid(&self) -> Vec3 {
        let n = self.vertices.len().max(1) as f32;
        self.vertices.iter().fold(Vec3::new(0.0, 0.0, 0.0), |a, &v| a + v) * (1.0 / n)
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::new(f32::MAX, f32::MAX, f32::MAX);
        let mut max = Vec3::new(f32::MIN, f32::MIN, f32::MIN);
        for &v in &self.vertices {
            min = min.min(v);
            max = max.max(v);
        }
        if self.vertices.is_empty() {
            (Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0))
        } else {
            (min, max)
        }
    }
}

fn set_bit(word: &mut u32, bit: u32, on: bool) {
    if on { *word |= bit } else { *word &= !bit }
}

/// A climb/drop/jump link between two polygons.
#[derive(Debug, Clone, PartialEq)]
pub struct NavPortal {
    pub kind: u8,
    pub angle: u8,
    pub flags_unk: u16,
    pub from: Vec3,
    pub to: Vec3,
    pub poly_from: u16,
    pub poly_to: u16,
    pub area_from: u16,
    pub area_to: u16,
    pub area_unk: u8,
}

/// A point of interest stored in the sector tree.
#[derive(Debug, Clone, PartialEq)]
pub struct NavPoint {
    pub position: Vec3,
    pub angle: u8,
    pub kind: u8,
}

/// A navmesh file.
#[derive(Debug, Clone, PartialEq)]
pub struct Ynv {
    pub content_flags: u32,
    /// Grid cell index (`y * 100 + x`), or 10000 for a standalone (vehicle) mesh.
    pub area_id: u32,
    /// Sector-tree root box; vertices are quantised inside it.
    pub bb_min: Vec3,
    pub bb_max: Vec3,
    /// `AABBSize`: the quantisation extent (normally `bb_max - bb_min`).
    pub bb_size: Vec3,
    /// `VersionUnk2`: 0x85CB3561 for grid cells, 0 for vehicles.
    pub version_unk2: u32,
    /// The root block's vtable value; kept so a rewritten file matches.
    pub vft: u32,
    pub polys: Vec<NavPoly>,
    pub portals: Vec<NavPortal>,
    pub points: Vec<NavPoint>,
}

/// The `NavMesh` root vtable in retail GTA V files.
pub const NAVMESH_VFT: u32 = 0x4061_E7E8;
const LIST_VFT_VERTICES: u32 = 1080158456;
const LIST_VFT_INDICES: u32 = 1080158424;
const LIST_VFT_EDGES: u32 = 1080158440;
const LIST_VFT_POLYS: u32 = 1080158408;

impl Ynv {
    /// An empty grid-cell navmesh for `area_id` covering `bb_min..bb_max`.
    pub fn new_cell(area_id: u32, bb_min: Vec3, bb_max: Vec3) -> Self {
        Self {
            content_flags: CONTENT_POLYGONS,
            area_id,
            bb_min,
            bb_max,
            bb_size: bb_max - bb_min,
            version_unk2: 0x85CB3561,
            vft: NAVMESH_VFT,
            polys: Vec::new(),
            portals: Vec::new(),
            points: Vec::new(),
        }
    }

    pub fn is_vehicle(&self) -> bool {
        self.content_flags & CONTENT_VEHICLE != 0
    }

    /// Grid cell coordinates for a map cell (`None` for standalone meshes).
    pub fn cell(&self) -> Option<(u32, u32)> {
        if self.area_id >= 10000 { None } else { Some((self.area_id % 100, self.area_id / 100)) }
    }
}

// ─── cell grid ───────────────────────────────────────────────────────────────

/// Grid geometry of GTA V's navmesh cells: 100×100 cells of 150 m starting
/// at (-6000, -6000). File names use the cell index times three.
pub const CELL_SIZE: f32 = 150.0;
pub const CELL_ORIGIN: f32 = -6000.0;

/// The cell (x, y) containing a world position.
pub fn cell_for_position(x: f32, y: f32) -> (u32, u32) {
    let cx = ((x - CELL_ORIGIN) / CELL_SIZE).floor().clamp(0.0, 99.0) as u32;
    let cy = ((y - CELL_ORIGIN) / CELL_SIZE).floor().clamp(0.0, 99.0) as u32;
    (cx, cy)
}

/// `navmesh[X][Y].ynv` for a cell.
pub fn cell_file_name(cx: u32, cy: u32) -> String {
    format!("navmesh[{}][{}].ynv", cx * 3, cy * 3)
}

/// World-space extent of a cell.
pub fn cell_bounds(cx: u32, cy: u32) -> (f32, f32, f32, f32) {
    let x0 = CELL_ORIGIN + cx as f32 * CELL_SIZE;
    let y0 = CELL_ORIGIN + cy as f32 * CELL_SIZE;
    (x0, y0, x0 + CELL_SIZE, y0 + CELL_SIZE)
}

// ─── reading ─────────────────────────────────────────────────────────────────

const POLY_SIZE: usize = 48;
const EDGE_SIZE: usize = 8;
const VERTEX_SIZE: usize = 6;
const INDEX_SIZE: usize = 2;
const PORTAL_SIZE: usize = 28;
const POINT_SIZE: usize = 8;
const NAVMESH_SIZE: usize = 368;
const LIST_SIZE: usize = 48;
const LIST_PART_SIZE: usize = 16;
const SECTOR_SIZE: usize = 96;
const SECTOR_DATA_SIZE: usize = 32;

/// Parses a `.ynv` (RSC7 bytes, deflated or stored).
pub fn parse_ynv(data: &[u8]) -> Result<Ynv> {
    let (system, graphics) = prepare_rsc7(data)?;
    let r = ResReader { system: &system, graphics: &graphics };
    let h = r.resolve(SYSTEM_BASE, NAVMESH_SIZE).context("NavMesh header")?;

    let vft = u32_le(h, 0x00);
    let content_flags = u32_le(h, 0x10);
    let bb_size = vec3_le(h, 0x60);
    let vertices_ptr = u64_le(h, 0x70);
    let indices_ptr = u64_le(h, 0x80);
    let edges_ptr = u64_le(h, 0x88);
    let adj_count = u32_le(h, 0x94) as usize;
    let adj_areas: Vec<u32> = (0..32).map(|i| u32_le(h, 0x98 + i * 4)).collect();
    let polys_ptr = u64_le(h, 0x118);
    let sector_ptr = u64_le(h, 0x120);
    let portals_ptr = u64_le(h, 0x128);
    let portal_links_ptr = u64_le(h, 0x130);
    let area_id = u32_le(h, 0x140);
    let portals_count = u32_le(h, 0x14C) as usize;
    let portal_links_count = u32_le(h, 0x150) as usize;
    let version_unk2 = u32_le(h, 0x160);

    let (bb_min, bb_max, raw_points) = read_sector_tree(&r, sector_ptr)?;
    let unquantise = |x: u16, y: u16, z: u16| -> Vec3 {
        const M: f32 = u16::MAX as f32;
        Vec3::new(
            bb_min.x + x as f32 / M * bb_size.x,
            bb_min.y + y as f32 / M * bb_size.y,
            bb_min.z + z as f32 / M * bb_size.z,
        )
    };

    let vertex_bytes = read_list(&r, vertices_ptr, VERTEX_SIZE).context("vertices")?;
    let vertices: Vec<Vec3> = vertex_bytes.chunks_exact(VERTEX_SIZE)
        .map(|c| unquantise(u16_le(c, 0), u16_le(c, 2), u16_le(c, 4)))
        .collect();
    let index_bytes = read_list(&r, indices_ptr, INDEX_SIZE).context("indices")?;
    let indices: Vec<u16> = index_bytes.chunks_exact(INDEX_SIZE).map(|c| u16_le(c, 0)).collect();
    let edge_bytes = read_list(&r, edges_ptr, EDGE_SIZE).context("edges")?;
    let area_of = |ind: u32| -> u32 {
        if (ind as usize) < adj_count { adj_areas[ind as usize] } else { ADJACENT_NONE }
    };
    let decode_end = |raw: u32| NavEdgeEnd {
        area_id: area_of(raw & 0x1F),
        poly: (raw >> 5) & 0x3FFF,
        unk2: ((raw >> 19) & 0x3) as u8,
        unk3: ((raw >> 21) & 0x7FF) as u16,
    };
    let edges: Vec<NavEdge> = edge_bytes.chunks_exact(EDGE_SIZE)
        .map(|c| NavEdge { a: decode_end(u32_le(c, 0)), b: decode_end(u32_le(c, 4)) })
        .collect();

    let portal_links: Vec<u16> = match r.resolve_optional(portal_links_ptr, portal_links_count * 2) {
        Some(Some(b)) => b.chunks_exact(2).map(|c| u16_le(c, 0)).collect(),
        _ => Vec::new(),
    };

    let poly_bytes = read_list(&r, polys_ptr, POLY_SIZE).context("polys")?;
    let mut polys = Vec::with_capacity(poly_bytes.len() / POLY_SIZE);
    for (pi, p) in poly_bytes.chunks_exact(POLY_SIZE).enumerate() {
        let flags0 = u16_le(p, 0x00);
        let count = (u16_le(p, 0x02) >> 5) as usize;
        let start = u16_le(p, 0x04) as usize;
        let flags1 = u32_le(p, 0x24);
        let flags2 = u32_le(p, 0x28);
        let part_flags = u32_le(p, 0x2C);
        let link_count = ((part_flags >> 12) & 0x7) as usize;
        let link_start = ((part_flags >> 15) & 0x1FFFF) as usize;

        if start + count > indices.len() || start + count > edges.len() {
            bail!("polygon {pi} indexes past the index/edge lists ({start}+{count})");
        }
        let mut vs = Vec::with_capacity(count);
        for &vi in &indices[start..start + count] {
            let v = *vertices.get(vi as usize).with_context(|| format!("polygon {pi}: vertex {vi} out of range"))?;
            vs.push(v);
        }
        let pls = portal_links.get(link_start..link_start + link_count).map(<[u16]>::to_vec).unwrap_or_default();
        polys.push(NavPoly { vertices: vs, edges: edges[start..start + count].to_vec(), flags0, flags1, flags2, portal_links: pls });
    }

    let mut portals = Vec::with_capacity(portals_count);
    if portals_count > 0 {
        let b = r.resolve(portals_ptr, portals_count * PORTAL_SIZE).context("portals")?;
        for c in b.chunks_exact(PORTAL_SIZE) {
            let area_flags = u32_le(c, 24);
            portals.push(NavPortal {
                kind: c[0],
                angle: c[1],
                flags_unk: u16_le(c, 2),
                from: unquantise(u16_le(c, 4), u16_le(c, 6), u16_le(c, 8)),
                to: unquantise(u16_le(c, 10), u16_le(c, 12), u16_le(c, 14)),
                poly_from: u16_le(c, 16),
                poly_to: u16_le(c, 20),
                area_from: (area_flags & 0x3FFF) as u16,
                area_to: ((area_flags >> 14) & 0x3FFF) as u16,
                area_unk: ((area_flags >> 28) & 0xF) as u8,
            });
        }
    }

    let points = raw_points.into_iter()
        .map(|(x, y, z, angle, kind)| NavPoint { position: unquantise(x, y, z), angle, kind })
        .collect();

    Ok(Ynv { content_flags, area_id, bb_min, bb_max, bb_size, version_unk2, vft, polys, portals, points })
}

/// Concatenated item bytes of a `NavMeshList<T>` (its parts in order).
fn read_list(r: &ResReader<'_>, va: u64, item_size: usize) -> Result<Vec<u8>> {
    if va == 0 {
        return Ok(Vec::new());
    }
    let h = r.resolve(va, LIST_SIZE).context("list header")?;
    let item_count = u32_le(h, 0x08) as usize;
    let parts_ptr = u64_le(h, 0x10);
    let parts_count = u32_le(h, 0x20) as usize;
    let mut out = Vec::with_capacity(item_count * item_size);
    if parts_count > 0 {
        let parts = r.resolve(parts_ptr, parts_count * LIST_PART_SIZE).context("list parts")?;
        for p in parts.chunks_exact(LIST_PART_SIZE) {
            let ptr = u64_le(p, 0);
            let count = u32_le(p, 8) as usize;
            if count == 0 { continue; }
            out.extend_from_slice(r.resolve(ptr, count * item_size).context("list part items")?);
        }
    }
    if out.len() != item_count * item_size {
        bail!("list holds {} items but its parts add up to {}", item_count, out.len() / item_size.max(1));
    }
    Ok(out)
}

/// Root box and every point in the sector tree (root first, then subtrees
/// 4,3,2,1 — the reference tool's stack order, kept so point indices agree).
fn read_sector_tree(r: &ResReader<'_>, root: u64) -> Result<(Vec3, Vec3, Vec<(u16, u16, u16, u8, u8)>)> {
    if root == 0 {
        bail!("navmesh has no sector tree");
    }
    let rb = r.resolve(root, SECTOR_SIZE).context("sector root")?;
    let bb_min = vec3_le(rb, 0);
    let bb_max = vec3_le(rb, 16);
    let mut points = Vec::new();
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(va) = stack.pop() {
        visited += 1;
        if visited > 4096 { bail!("sector tree too large or cyclic"); }
        let s = r.resolve(va, SECTOR_SIZE).context("sector")?;
        let data_ptr = u64_le(s, 0x2C);
        if data_ptr != 0 {
            let d = r.resolve(data_ptr, SECTOR_DATA_SIZE).context("sector data")?;
            let points_ptr = u64_le(d, 0x10);
            let points_count = u16_le(d, 0x1A) as usize;
            if points_count > 0 {
                let pb = r.resolve(points_ptr, points_count * POINT_SIZE).context("sector points")?;
                for c in pb.chunks_exact(POINT_SIZE) {
                    points.push((u16_le(c, 0), u16_le(c, 2), u16_le(c, 4), c[6], c[7]));
                }
            }
        }
        for i in 0..4 {
            let sub = u64_le(s, 0x34 + i * 8);
            if sub != 0 { stack.push(sub); }
        }
    }
    Ok((bb_min, bb_max, points))
}

// ─── writing ─────────────────────────────────────────────────────────────────

/// Serialises `ynv` to RSC7 bytes (version 2, deflated), rebuilding the
/// vertex/index/edge lists, the adjacent-area table, the portal links and
/// the sector tree from the polygon data, as the reference tool's `Save()` does.
pub fn serialize_ynv(ynv: &Ynv) -> Result<Vec<u8>> {
    if ynv.polys.len() > 0x3FFF {
        bail!("{} polygons; a cell holds at most 16383", ynv.polys.len());
    }
    let bb_min = ynv.bb_min;
    let size = ynv.bb_size;
    let quantise = |v: Vec3| -> [u16; 3] {
        const M: f32 = u16::MAX as f32;
        let q = |a: f32, lo: f32, extent: f32| -> u16 {
            let t = if extent.abs() < f32::EPSILON { 0.0 } else { ((a - lo) / extent).clamp(0.0, 1.0) };
            (t * M).round() as u16
        };
        [q(v.x, bb_min.x, size.x), q(v.y, bb_min.y, size.y), q(v.z, bb_min.z, size.z)]
    };

    // Adjacent-area table: the reference tool seeds it with self, none and the four
    // neighbours in this order, then appends whatever the edges mention.
    let mut areas: Vec<u32> = Vec::new();
    let mut area_index: HashMap<u32, u32> = HashMap::new();
    let ensure_area = |id: u32, areas: &mut Vec<u32>, index: &mut HashMap<u32, u32>| -> Result<u32> {
        if let Some(&i) = index.get(&id) { return Ok(i); }
        if areas.len() >= 32 { bail!("more than 32 distinct adjacent areas"); }
        let i = areas.len() as u32;
        areas.push(id);
        index.insert(id, i);
        Ok(i)
    };
    let a = ynv.area_id;
    for id in [a, ADJACENT_NONE, a.wrapping_sub(100), a.wrapping_sub(1), a + 1, a + 100] {
        ensure_area(id, &mut areas, &mut area_index)?;
    }

    let mut vertex_index: HashMap<[u16; 3], u16> = HashMap::new();
    let mut vertices: Vec<[u16; 3]> = Vec::new();
    let mut indices: Vec<u16> = Vec::new();
    let mut edges: Vec<[u32; 2]> = Vec::new();
    let mut portal_links: Vec<u16> = Vec::new();
    let mut poly_records: Vec<[u8; POLY_SIZE]> = Vec::with_capacity(ynv.polys.len());
    let mut poly_cells: Vec<[i16; 6]> = Vec::with_capacity(ynv.polys.len());
    let polys_per_part = 16384 / POLY_SIZE;

    for (pi, poly) in ynv.polys.iter().enumerate() {
        let count = poly.vertices.len();
        if count < 3 || count > 0x7FF {
            bail!("polygon {pi} has {count} vertices");
        }
        let start = indices.len();
        if start > u16::MAX as usize {
            bail!("more than 65535 polygon vertices in one cell");
        }
        for (vi, &v) in poly.vertices.iter().enumerate() {
            let q = quantise(v);
            let index = match vertex_index.get(&q) {
                Some(&i) => i,
                None => {
                    if vertices.len() > u16::MAX as usize { bail!("more than 65536 distinct vertices"); }
                    let i = vertices.len() as u16;
                    vertices.push(q);
                    vertex_index.insert(q, i);
                    i
                }
            };
            indices.push(index);
            let edge = poly.edges.get(vi).copied().unwrap_or(NavEdge::NONE);
            let mut encode = |end: NavEdgeEnd| -> Result<u32> {
                let ai = ensure_area(end.area_id, &mut areas, &mut area_index)?;
                Ok((ai & 0x1F) | ((end.poly & 0x3FFF) << 5) | ((end.unk2 as u32 & 0x3) << 19) | ((end.unk3 as u32 & 0x7FF) << 21))
            };
            edges.push([encode(edge.a)?, encode(edge.b)?]);
        }

        let link_start = portal_links.len();
        if poly.portal_links.len() > 7 { bail!("polygon {pi} has more than 7 portal links"); }
        portal_links.extend_from_slice(&poly.portal_links);

        let (min, max) = poly.bounds();
        let cell = [
            (min.x * 4.0).floor() as i16, (max.x * 4.0).ceil() as i16,
            (min.y * 4.0).floor() as i16, (max.y * 4.0).ceil() as i16,
            (min.z * 4.0).floor() as i16, (max.z * 4.0).ceil() as i16,
        ];
        poly_cells.push(cell);

        let mut rec = [0u8; POLY_SIZE];
        rec[0x00..0x02].copy_from_slice(&poly.flags0.to_le_bytes());
        rec[0x02..0x04].copy_from_slice(&((count as u16) << 5).to_le_bytes());
        rec[0x04..0x06].copy_from_slice(&(start as u16).to_le_bytes());
        rec[0x06..0x08].copy_from_slice(&(ynv.area_id as u16).to_le_bytes());
        for (i, c) in cell.iter().enumerate() {
            rec[0x18 + i * 2..0x1A + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        rec[0x24..0x28].copy_from_slice(&poly.flags1.to_le_bytes());
        rec[0x28..0x2C].copy_from_slice(&poly.flags2.to_le_bytes());
        let part_id = (pi / polys_per_part) as u32;
        let part_flags = ((part_id & 0xFF) << 4)
            | ((poly.portal_links.len() as u32 & 0x7) << 12)
            | ((link_start as u32 & 0x1FFFF) << 15);
        rec[0x2C..0x30].copy_from_slice(&part_flags.to_le_bytes());
        poly_records.push(rec);
    }

    // ── lay the blocks out ──
    let mut w = BlockWriter::new();
    let main = w.alloc(NAVMESH_SIZE);

    let vertex_bytes: Vec<u8> = vertices.iter().flat_map(|q| q.iter().flat_map(|c| c.to_le_bytes())).collect();
    let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let edge_bytes: Vec<u8> = edges.iter().flat_map(|e| e.iter().flat_map(|c| c.to_le_bytes())).collect();
    let poly_bytes: Vec<u8> = poly_records.iter().flat_map(|r| r.iter().copied()).collect();
    let vertices_block = w.write_list(LIST_VFT_VERTICES, &vertex_bytes, VERTEX_SIZE);
    let indices_block = w.write_list(LIST_VFT_INDICES, &index_bytes, INDEX_SIZE);
    let edges_block = w.write_list(LIST_VFT_EDGES, &edge_bytes, EDGE_SIZE);
    let polys_block = w.write_list(LIST_VFT_POLYS, &poly_bytes, POLY_SIZE);

    // Sector tree: two levels of quadtree for map cells, a single node for vehicles.
    let depth = if ynv.is_vehicle() { 0 } else { 2 };
    let quantised_points: Vec<([u16; 3], u8, u8)> = ynv.points.iter().map(|p| (quantise(p.position), p.angle, p.kind)).collect();
    let mut point_used = vec![false; ynv.points.len()];
    let mut point_index = 0u32;
    let mut total_bytes = 0u32;
    let sector_root = write_sector(
        &mut w, ynv.bb_min, ynv.bb_max, depth, &poly_cells, &ynv.points, &quantised_points,
        &mut point_used, &mut point_index, &mut total_bytes,
    );
    total_bytes += (vertex_bytes.len() + index_bytes.len() + edge_bytes.len() + poly_bytes.len()) as u32;
    total_bytes += ynv.portals.len() as u32 * PORTAL_SIZE as u32;

    let portals_block = if ynv.portals.is_empty() { 0 } else {
        let mut b = Vec::with_capacity(ynv.portals.len() * PORTAL_SIZE);
        for p in &ynv.portals {
            b.push(p.kind);
            b.push(p.angle);
            b.extend_from_slice(&p.flags_unk.to_le_bytes());
            for c in quantise(p.from) { b.extend_from_slice(&c.to_le_bytes()); }
            for c in quantise(p.to) { b.extend_from_slice(&c.to_le_bytes()); }
            for v in [p.poly_from, p.poly_from, p.poly_to, p.poly_to] { b.extend_from_slice(&v.to_le_bytes()); }
            let area_flags = (p.area_from as u32 & 0x3FFF) | ((p.area_to as u32 & 0x3FFF) << 14) | ((p.area_unk as u32 & 0xF) << 28);
            b.extend_from_slice(&area_flags.to_le_bytes());
        }
        w.write_block(&b)
    };
    let links_block = if portal_links.is_empty() { 0 } else {
        let b: Vec<u8> = portal_links.iter().flat_map(|l| l.to_le_bytes()).collect();
        w.write_block(&b)
    };

    // ── main block ──
    let mut h = [0u8; NAVMESH_SIZE];
    put_u32(&mut h, 0x00, ynv.vft);
    put_u32(&mut h, 0x04, 1);
    put_u32(&mut h, 0x10, ynv.content_flags);
    put_u32(&mut h, 0x14, 0x0001_0011);
    // Identity transform with NaN in the w column, as retail files carry.
    for row in 0..4 {
        for col in 0..4 {
            let v = if col == 3 { f32::NAN } else if row == col { 1.0 } else { 0.0 };
            put_f32(&mut h, 0x20 + row * 16 + col * 4, v);
        }
    }
    put_vec3(&mut h, 0x60, ynv.bb_size);
    put_u32(&mut h, 0x6C, 0x7F80_0001);
    put_u64(&mut h, 0x70, vertices_block);
    put_u64(&mut h, 0x80, indices_block);
    put_u64(&mut h, 0x88, edges_block);
    put_u32(&mut h, 0x90, indices.len() as u32);
    put_u32(&mut h, 0x94, areas.len() as u32);
    for (i, &id) in areas.iter().enumerate() {
        put_u32(&mut h, 0x98 + i * 4, id);
    }
    put_u64(&mut h, 0x118, polys_block);
    put_u64(&mut h, 0x120, sector_root);
    put_u64(&mut h, 0x128, portals_block);
    put_u64(&mut h, 0x130, links_block);
    put_u32(&mut h, 0x138, vertices.len() as u32);
    put_u32(&mut h, 0x13C, ynv.polys.len() as u32);
    put_u32(&mut h, 0x140, ynv.area_id);
    put_u32(&mut h, 0x144, total_bytes);
    put_u32(&mut h, 0x148, point_index);
    put_u32(&mut h, 0x14C, ynv.portals.len() as u32);
    put_u32(&mut h, 0x150, portal_links.len() as u32);
    put_u32(&mut h, 0x160, ynv.version_unk2);
    w.patch(main, &h);

    build_rsc7_paged(2, &w.finish(), PAGE_SIZE)
}

#[allow(clippy::too_many_arguments)]
fn write_sector(
    w: &mut BlockWriter, min: Vec3, max: Vec3, depth: u32, poly_cells: &[[i16; 6]],
    points: &[NavPoint], quantised: &[([u16; 3], u8, u8)], point_used: &mut [bool],
    point_index: &mut u32, total_bytes: &mut u32,
) -> u64 {
    let node = w.alloc(SECTOR_SIZE);
    let cell = [
        (min.x * 4.0).floor() as i16, (max.x * 4.0).ceil() as i16,
        (min.y * 4.0).floor() as i16, (max.y * 4.0).ceil() as i16,
        (min.z * 4.0).floor() as i16, (max.z * 4.0).ceil() as i16,
    ];
    let mut s = [0u8; SECTOR_SIZE];
    put_vec3(&mut s, 0, min); put_f32(&mut s, 12, f32::NAN);
    put_vec3(&mut s, 16, max); put_f32(&mut s, 28, f32::NAN);
    for (i, c) in cell.iter().enumerate() {
        s[32 + i * 2..34 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    *total_bytes += SECTOR_SIZE as u32;

    if depth == 0 {
        let poly_ids: Vec<u16> = poly_cells.iter().enumerate()
            .filter(|(_, b)| b[1] >= cell[0] && b[0] <= cell[1] && b[3] >= cell[2] && b[2] <= cell[3])
            .map(|(i, _)| i as u16)
            .collect();
        let mut sector_points: Vec<u8> = Vec::new();
        let mut count = 0u16;
        for (i, p) in points.iter().enumerate() {
            if point_used[i] { continue; }
            let pos = p.position;
            if pos.x >= min.x && pos.x <= max.x && pos.y >= min.y && pos.y <= max.y {
                point_used[i] = true;
                let (q, angle, kind) = quantised[i];
                for c in q { sector_points.extend_from_slice(&c.to_le_bytes()); }
                sector_points.push(angle);
                sector_points.push(kind);
                count += 1;
            }
        }
        let mut d = [0u8; SECTOR_DATA_SIZE];
        put_u32(&mut d, 0, *point_index);
        if !poly_ids.is_empty() {
            let b: Vec<u8> = poly_ids.iter().flat_map(|i| i.to_le_bytes()).collect();
            put_u64(&mut d, 0x08, w.write_block(&b));
        }
        if count > 0 {
            put_u64(&mut d, 0x10, w.write_block(&sector_points));
        }
        d[0x18..0x1A].copy_from_slice(&(poly_ids.len() as u16).to_le_bytes());
        d[0x1A..0x1C].copy_from_slice(&count.to_le_bytes());
        *point_index += count as u32;
        *total_bytes += SECTOR_DATA_SIZE as u32 + poly_ids.len() as u32 * 2 + count as u32 * 8;
        let data = w.write_block(&d);
        put_u64(&mut s, 0x2C, data);
    } else {
        let cen = (min + max) * 0.5;
        // Clockwise from +XY; the odd z values are what retail files carry.
        let quads = [
            (Vec3::new(cen.x, cen.y, cen.z), Vec3::new(max.x, max.y, max.z)),
            (Vec3::new(cen.x, min.y, 0.0), Vec3::new(max.x, cen.y, 0.0)),
            (Vec3::new(min.x, min.y, min.z), Vec3::new(cen.x, cen.y, cen.z)),
            (Vec3::new(min.x, cen.y, 0.0), Vec3::new(cen.x, max.y, 0.0)),
        ];
        for (i, (qmin, qmax)) in quads.iter().enumerate() {
            let sub = write_sector(w, *qmin, *qmax, depth - 1, poly_cells, points, quantised, point_used, point_index, total_bytes);
            put_u64(&mut s, 0x34 + i * 8, sub);
        }
    }
    w.patch(node, &s);
    node
}

/// System-section page size used for written navmeshes. The game maps each
/// RSC7 page as its own allocation and relocates pointers page by page, so a
/// block must never straddle a page boundary; 16 KiB is the largest block a
/// navmesh list part can be.
const PAGE_SIZE: usize = 16384;

/// Page-aware, 16-byte-aligned system-section layout with pointer patching.
struct BlockWriter {
    buf: Vec<u8>,
}

impl BlockWriter {
    fn new() -> Self { Self { buf: Vec::new() } }

    /// Reserves `size` zeroed bytes, never across a page boundary, and
    /// returns their virtual address.
    fn alloc(&mut self, size: usize) -> u64 {
        assert!(size <= PAGE_SIZE, "block of {size} bytes exceeds the {PAGE_SIZE}-byte page");
        while self.buf.len() % 16 != 0 { self.buf.push(0); }
        let in_page = self.buf.len() % PAGE_SIZE;
        if in_page + size > PAGE_SIZE {
            let pad = PAGE_SIZE - in_page;
            self.buf.resize(self.buf.len() + pad, 0);
        }
        let off = self.buf.len();
        self.buf.resize(off + size, 0);
        SYSTEM_BASE + off as u64
    }

    fn write_block(&mut self, bytes: &[u8]) -> u64 {
        let va = self.alloc(bytes.len());
        self.patch(va, bytes);
        va
    }

    fn patch(&mut self, va: u64, bytes: &[u8]) {
        let off = (va - SYSTEM_BASE) as usize;
        self.buf[off..off + bytes.len()].copy_from_slice(bytes);
    }

    /// A `NavMeshList<T>`: header, parts of at most 16 KiB, offsets table.
    fn write_list(&mut self, vft: u32, items: &[u8], item_size: usize) -> u64 {
        let header = self.alloc(LIST_SIZE);
        let per_part = 16384 / item_size;
        let item_count = items.len() / item_size;
        let parts: Vec<&[u8]> = items.chunks(per_part * item_size).collect();
        let parts_array = self.alloc(parts.len() * LIST_PART_SIZE);
        let mut offsets = Vec::with_capacity(parts.len());
        let mut part_recs = Vec::with_capacity(parts.len() * LIST_PART_SIZE);
        let mut first = 0usize;
        for part in &parts {
            let va = self.write_block(part);
            let mut rec = [0u8; LIST_PART_SIZE];
            put_u64(&mut rec, 0, va);
            put_u32(&mut rec, 8, (part.len() / item_size) as u32);
            part_recs.extend_from_slice(&rec);
            offsets.extend_from_slice(&(first as u32).to_le_bytes());
            first += part.len() / item_size;
        }
        self.patch(parts_array, &part_recs);
        let offsets_block = if offsets.is_empty() { 0 } else { self.write_block(&offsets) };

        let mut h = [0u8; LIST_SIZE];
        put_u32(&mut h, 0x00, vft);
        put_u32(&mut h, 0x04, 1);
        put_u32(&mut h, 0x08, item_count as u32);
        put_u64(&mut h, 0x10, parts_array);
        put_u64(&mut h, 0x18, offsets_block);
        put_u32(&mut h, 0x20, parts.len() as u32);
        self.patch(header, &h);
        header
    }

    fn finish(self) -> Vec<u8> { self.buf }
}

fn put_u32(b: &mut [u8], off: usize, v: u32) { b[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
fn put_u64(b: &mut [u8], off: usize, v: u64) { b[off..off + 8].copy_from_slice(&v.to_le_bytes()); }
fn put_f32(b: &mut [u8], off: usize, v: f32) { b[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
fn put_vec3(b: &mut [u8], off: usize, v: Vec3) { put_f32(b, off, v.x); put_f32(b, off + 4, v.y); put_f32(b, off + 8, v.z); }

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f32, y: f32, z: f32, s: f32) -> Vec<Vec3> {
        vec![Vec3::new(x, y, z), Vec3::new(x + s, y, z), Vec3::new(x + s, y + s, z), Vec3::new(x, y + s, z)]
    }

    #[test]
    fn cell_maths_match_reference() {
        assert_eq!(cell_for_position(-578.5, -1061.5), (36, 32));
        assert_eq!(cell_file_name(36, 32), "navmesh[108][96].ynv");
        assert_eq!(cell_bounds(36, 32), (-600.0, -1050.0 - 150.0, -450.0, -1050.0));
    }

    #[test]
    fn two_polygons_round_trip() {
        let mut ynv = Ynv::new_cell(3236, Vec3::new(-600.0, -1200.0, -100.0), Vec3::new(-450.0, -1050.0, 200.0));
        let mut a = NavPoly::new(square(-580.0, -1070.0, 22.4, 2.0));
        let mut b = NavPoly::new(square(-578.0, -1070.0, 22.4, 2.0));
        a.set_interior(true);
        a.set_flat_ground(true);
        b.flags0 = 4;
        a.edges[1] = NavEdge::neighbour(3236, 1);
        b.edges[3] = NavEdge::neighbour(3236, 0);
        b.edges[0] = NavEdge::neighbour(3136, 77);
        ynv.polys = vec![a, b];
        ynv.points.push(NavPoint { position: Vec3::new(-579.0, -1069.0, 22.4), angle: 12, kind: 3 });
        ynv.portals.push(NavPortal {
            kind: 1, angle: 9, flags_unk: 0, from: Vec3::new(-579.0, -1069.0, 22.4), to: Vec3::new(-577.0, -1069.0, 22.4),
            poly_from: 0, poly_to: 1, area_from: 3236, area_to: 3236, area_unk: 0,
        });
        ynv.polys[0].portal_links = vec![0];

        let bytes = serialize_ynv(&ynv).unwrap();
        let back = parse_ynv(&bytes).unwrap();

        assert_eq!(back.area_id, 3236);
        assert_eq!(back.content_flags, ynv.content_flags);
        assert_eq!(back.polys.len(), 2);
        assert!(back.polys[0].is_interior() && back.polys[0].is_flat_ground());
        assert_eq!(back.polys[1].flags0, 4);
        assert_eq!(back.polys[0].edges[1], NavEdge::neighbour(3236, 1));
        assert_eq!(back.polys[1].edges[0], NavEdge::neighbour(3136, 77));
        assert_eq!(back.polys[1].edges[1], NavEdge::NONE);
        assert_eq!(back.polys[0].portal_links, vec![0]);
        assert_eq!(back.points.len(), 1);
        assert_eq!(back.points[0].kind, 3);
        assert_eq!(back.portals.len(), 1);
        assert_eq!(back.portals[0].poly_to, 1);
        // Quantisation error inside a 150 m cell is well under 5 mm.
        for (p, q) in ynv.polys.iter().zip(&back.polys) {
            for (v, w) in p.vertices.iter().zip(&q.vertices) {
                assert!((*v - *w).length() < 0.005, "{v:?} vs {w:?}");
            }
        }
        assert!((back.points[0].position - ynv.points[0].position).length() < 0.01);

        // Shared vertices are written once.
        let again = serialize_ynv(&back).unwrap();
        assert_eq!(parse_ynv(&again).unwrap().polys, back.polys);
    }

    #[test]
    fn empty_cell_is_valid() {
        let ynv = Ynv::new_cell(1, Vec3::new(0.0, 0.0, 0.0), Vec3::new(150.0, 150.0, 100.0));
        let back = parse_ynv(&serialize_ynv(&ynv).unwrap()).unwrap();
        assert!(back.polys.is_empty());
        assert_eq!(back.bb_max, ynv.bb_max);
    }
}
