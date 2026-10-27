//! Path node files (`.ynd`), read and write: the road network vehicles and
//! pedestrians drive and walk along, one file per 512 m grid cell. A cell
//! holds nodes (a position quantised to 1/4 m in x and y and 1/32 m in z, a
//! street name hash and five flag bytes), the links leaving each node (to a
//! node in this or another cell, with lane counts and a length) and the
//! junction heightmaps some nodes carry.
//!
//! Ported from the reference library's `Resources/Node.cs` (`NodeDictionary`,
//! `Node`, `NodeLink`, `NodeJunction`, `NodeJunctionRef`) and
//! `FileTypes/YndFile.cs` (`YndNode`/`YndLink` flag accessors, `YndXml` and
//! `XmlYnd`). [`Ynd`] is the file as the reference tool's XML lays it out: every
//! node with its own links inline, the junctions with their heightmaps
//! inline, and the junction refs; [`NodeDictionary`] is the raw block. The
//! writer lays the blocks out with the same block graph
//! `ResourceBuilder.Build` uses, so what it writes is what the reference tool
//! writes.

use anyhow::{bail, Context, Result};

use crate::blocks::base::{write_file_base, PagesInfo, StructArray};
use crate::blocks::xml::{child, child_attr_f32, child_attr_u32, child_text, child_vec3, hash_string, items, parse_hash, write_items, XmlOut};
use crate::blocks::{count_u16, Block, BlockId, Graph, Pod, Reader, Writer};
use crate::math::{Vec2, Vec3};
use crate::names::NameTable;
use crate::resource::SYSTEM_BASE;

// ─── cell grid ───────────────────────────────────────────────────────────────

/// Grid geometry of the path node cells (`SpaceNodeGrid`): 32×32 cells of
/// 512 m from (-8192, -8192), each 4096 m tall from z = -2048. A cell's
/// area id is `y * 32 + x` and its file is `nodes<area>.ynd`.
pub const CELL_SIZE: f32 = 512.0;
pub const CELL_ORIGIN: f32 = -8192.0;
pub const CELL_COUNT: u32 = 32;
pub const CELL_MIN_Z: f32 = -2048.0;
pub const CELL_HEIGHT: f32 = 4096.0;

/// The cell (x, y) containing a world position, clamped to the grid.
pub fn cell_for_position(x: f32, y: f32) -> (u32, u32) {
    let max = (CELL_COUNT - 1) as f32;
    let cx = ((x - CELL_ORIGIN) / CELL_SIZE).floor().clamp(0.0, max) as u32;
    let cy = ((y - CELL_ORIGIN) / CELL_SIZE).floor().clamp(0.0, max) as u32;
    (cx, cy)
}

/// A cell's area id, `y * 32 + x`.
pub fn area_id(cx: u32, cy: u32) -> u32 {
    cy * CELL_COUNT + cx
}

/// Bit 10 of an area id marks a Cayo Perico cell (`nodes1177.ynd` to
/// `nodes1274.ynd` in `update.rpf`): the island's nodes stream in over the
/// same grid cells as the sea they replace, and their links name the cell
/// without the flag. The reference tool's 32×32 grid does not load these.
pub const ISLAND_FLAG: u32 = 1024;

/// Whether an area id carries the island flag.
pub fn is_island_area(area: u32) -> bool {
    area & ISLAND_FLAG != 0
}

/// Whether two area ids name the same grid cell, the island flag aside:
/// an island cell's own links name it without the flag.
pub fn same_cell(a: u32, b: u32) -> bool {
    a & (ISLAND_FLAG - 1) == b & (ISLAND_FLAG - 1)
}

/// The cell (x, y) an area id names, the island flag aside.
pub fn cell_of_area(area: u32) -> (u32, u32) {
    let area = area & (ISLAND_FLAG - 1);
    (area % CELL_COUNT, area / CELL_COUNT)
}

/// `nodes<area>.ynd` for a cell.
pub fn cell_file_name(cx: u32, cy: u32) -> String {
    format!("nodes{}.ynd", area_id(cx, cy))
}

/// World-space `(x0, y0, x1, y1)` of a cell.
pub fn cell_bounds(cx: u32, cy: u32) -> (f32, f32, f32, f32) {
    let x0 = CELL_ORIGIN + cx as f32 * CELL_SIZE;
    let y0 = CELL_ORIGIN + cy as f32 * CELL_SIZE;
    (x0, y0, x0 + CELL_SIZE, y0 + CELL_SIZE)
}

/// The area id a file name carries: `nodes489.ynd` (in any case, with any
/// path in front) is 489. `YndFile.Load` reads it the same way.
pub fn area_id_from_file_name(name: &str) -> Option<u32> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    base.strip_prefix("nodes")?.strip_suffix(".ynd")?.parse().ok()
}

// ─── raw structs ─────────────────────────────────────────────────────────────

/// `Node` (40 bytes): one path node as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Node {
    pub unused0: u32,
    pub unused1: u32,
    pub unused2: u32,
    pub unused3: u32,
    pub area_id: u16,
    pub node_id: u16,
    /// A text (GXT) hash naming the street.
    pub street_name: u32,
    pub unused4: u16,
    /// Index of this node's first link in the file's link list.
    pub link_id: u16,
    /// World x and y times 4.
    pub position_x: i16,
    pub position_y: i16,
    pub flags0: u8,
    pub flags1: u8,
    /// World z times 32.
    pub position_z: i16,
    pub flags2: u8,
    /// Link count in the top five bits, `Flags5` in the low three.
    pub link_count_flags: u8,
    pub flags3: u8,
    pub flags4: u8,
}

impl Pod for Node {
    const SIZE: usize = 40;
    fn write(&self, w: &mut Writer) {
        w.u32(self.unused0); w.u32(self.unused1); w.u32(self.unused2); w.u32(self.unused3);
        w.u16(self.area_id); w.u16(self.node_id); w.u32(self.street_name);
        w.u16(self.unused4); w.u16(self.link_id);
        w.i16(self.position_x); w.i16(self.position_y);
        w.u8(self.flags0); w.u8(self.flags1);
        w.i16(self.position_z);
        w.u8(self.flags2); w.u8(self.link_count_flags); w.u8(self.flags3); w.u8(self.flags4);
    }
    fn read(b: &[u8]) -> Self {
        Self {
            unused0: u32::read(&b[0..]), unused1: u32::read(&b[4..]), unused2: u32::read(&b[8..]), unused3: u32::read(&b[12..]),
            area_id: u16::read(&b[16..]), node_id: u16::read(&b[18..]), street_name: u32::read(&b[20..]),
            unused4: u16::read(&b[24..]), link_id: u16::read(&b[26..]),
            position_x: i16::read(&b[28..]), position_y: i16::read(&b[30..]),
            flags0: b[32], flags1: b[33],
            position_z: i16::read(&b[34..]),
            flags2: b[36], link_count_flags: b[37], flags3: b[38], flags4: b[39],
        }
    }
}

/// `NodeLink` (8 bytes): one link as stored, naming the node it leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeLink {
    pub area_id: u16,
    pub node_id: u16,
    pub flags0: u8,
    pub flags1: u8,
    pub flags2: u8,
    pub link_length: u8,
}

impl Pod for NodeLink {
    const SIZE: usize = 8;
    fn write(&self, w: &mut Writer) {
        w.u16(self.area_id); w.u16(self.node_id);
        w.u8(self.flags0); w.u8(self.flags1); w.u8(self.flags2); w.u8(self.link_length);
    }
    fn read(b: &[u8]) -> Self {
        Self { area_id: u16::read(b), node_id: u16::read(&b[2..]), flags0: b[4], flags1: b[5], flags2: b[6], link_length: b[7] }
    }
}

/// `NodeJunction` (12 bytes): a junction's footprint and where its
/// heightmap starts in the file's heightmap bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeJunction {
    pub max_z: i16,
    pub position_x: i16,
    pub position_y: i16,
    pub min_z: i16,
    pub heightmap_ptr: u16,
    pub heightmap_dim_x: u8,
    pub heightmap_dim_y: u8,
}

impl Pod for NodeJunction {
    const SIZE: usize = 12;
    fn write(&self, w: &mut Writer) {
        w.i16(self.max_z); w.i16(self.position_x); w.i16(self.position_y); w.i16(self.min_z);
        w.u16(self.heightmap_ptr); w.u8(self.heightmap_dim_x); w.u8(self.heightmap_dim_y);
    }
    fn read(b: &[u8]) -> Self {
        Self {
            max_z: i16::read(b), position_x: i16::read(&b[2..]), position_y: i16::read(&b[4..]), min_z: i16::read(&b[6..]),
            heightmap_ptr: u16::read(&b[8..]), heightmap_dim_x: b[10], heightmap_dim_y: b[11],
        }
    }
}

/// `NodeJunctionRef` (8 bytes): which node a junction belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeJunctionRef {
    pub area_id: u16,
    pub node_id: u16,
    pub junction_id: u16,
    pub unk0: u16,
}

impl Pod for NodeJunctionRef {
    const SIZE: usize = 8;
    fn write(&self, w: &mut Writer) { w.u16(self.area_id); w.u16(self.node_id); w.u16(self.junction_id); w.u16(self.unk0); }
    fn read(b: &[u8]) -> Self {
        Self { area_id: u16::read(b), node_id: u16::read(&b[2..]), junction_id: u16::read(&b[4..]), unk0: u16::read(&b[6..]) }
    }
}

/// `NodeDictionary`: the root block (112 bytes) and the five arrays it
/// points at, as stored. The `unk*` words are named by their offset, as
/// The reference tool names them; retail files carry 0 in all but `unk48`, which is 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDictionary {
    pub vft: u32,
    pub nodes_count_vehicle: u32,
    pub nodes_count_ped: u32,
    pub unk24: u32,
    pub unk34: u32,
    pub unk48: u32,
    pub unk4c: u32,
    pub unk5c: u32,
    pub unk68: u32,
    pub unk6c: u32,
    pub nodes: Vec<Node>,
    pub links: Vec<NodeLink>,
    pub junctions: Vec<NodeJunction>,
    pub junction_heightmap_bytes: Vec<u8>,
    pub junction_refs: Vec<NodeJunctionRef>,
}

/// The `NodeDictionary` vtable value the base game's files carry (a
/// vtable address, so it differs by build; the game ignores it).
pub const NODE_DICTIONARY_VFT: u32 = 0x4061_E3B0;

impl Default for NodeDictionary {
    fn default() -> Self {
        Self {
            vft: NODE_DICTIONARY_VFT,
            nodes_count_vehicle: 0, nodes_count_ped: 0,
            unk24: 0, unk34: 0, unk48: 1, unk4c: 0, unk5c: 0, unk68: 0, unk6c: 0,
            nodes: Vec::new(), links: Vec::new(), junctions: Vec::new(),
            junction_heightmap_bytes: Vec::new(), junction_refs: Vec::new(),
        }
    }
}

const DICTIONARY_SIZE: usize = 112;

/// `NodeDictionary.Read`: the root block and its arrays.
pub fn parse_node_dictionary(data: &[u8]) -> Result<NodeDictionary> {
    let r = Reader::open(data)?;
    let mut c = r.cursor(SYSTEM_BASE).context("NodeDictionary")?;
    let vft = c.u32();
    let _unknown = c.u32();
    let _pages = c.u64();
    let nodes_ptr = c.u64();
    let nodes_count = c.u32() as usize;
    let nodes_count_vehicle = c.u32();
    let nodes_count_ped = c.u32();
    let unk24 = c.u32();
    let links_ptr = c.u64();
    let links_count = c.u32() as usize;
    let unk34 = c.u32();
    let junctions_ptr = c.u64();
    let heightmap_ptr = c.u64();
    let unk48 = c.u32();
    let unk4c = c.u32();
    let refs_ptr = c.u64();
    let _refs_count0 = c.u16();
    let refs_count1 = c.u16() as usize;
    let unk5c = c.u32();
    let junctions_count = c.u32() as usize;
    let heightmap_count = c.u32() as usize;
    let unk68 = c.u32();
    let unk6c = c.u32();
    c.check().context("NodeDictionary header")?;

    Ok(NodeDictionary {
        vft, nodes_count_vehicle, nodes_count_ped, unk24, unk34, unk48, unk4c, unk5c, unk68, unk6c,
        nodes: r.structs::<Node>(nodes_ptr, nodes_count).context("nodes")?,
        links: r.structs::<NodeLink>(links_ptr, links_count).context("links")?,
        junctions: r.structs::<NodeJunction>(junctions_ptr, junctions_count).context("junctions")?,
        junction_heightmap_bytes: r.bytes(heightmap_ptr, heightmap_count).context("junction heightmaps")?,
        junction_refs: r.structs::<NodeJunctionRef>(refs_ptr, refs_count1).context("junction refs")?,
    })
}

/// The root block: `NodeDictionary.Write`, with its references in
/// `GetReferences` order (junction refs, heightmap bytes, junctions, links,
/// nodes), which is the order the pages are packed in.
struct DictionaryBlock {
    d: NodeDictionary,
    pages: BlockId,
    nodes: Option<BlockId>,
    links: Option<BlockId>,
    junctions: Option<BlockId>,
    heightmap: Option<BlockId>,
    refs: Option<BlockId>,
}

impl Block for DictionaryBlock {
    fn length(&self) -> usize { DICTIONARY_SIZE }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        std::iter::once(Some(self.pages))
            .chain([self.refs, self.heightmap, self.junctions, self.links, self.nodes])
            .flatten()
            .collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let d = &self.d;
        write_file_base(w, g, d.vft, Some(self.pages));
        w.u64(g.ptr(self.nodes));
        w.u32(d.nodes.len() as u32);
        w.u32(d.nodes_count_vehicle);
        w.u32(d.nodes_count_ped);
        w.u32(d.unk24);
        w.u64(g.ptr(self.links));
        w.u32(d.links.len() as u32);
        w.u32(d.unk34);
        w.u64(g.ptr(self.junctions));
        w.u64(g.ptr(self.heightmap));
        w.u32(d.unk48);
        w.u32(d.unk4c);
        w.u64(g.ptr(self.refs));
        let refs = count_u16(d.junction_refs.len(), "junction refs")?;
        w.u16(refs);
        w.u16(refs);
        w.u32(d.unk5c);
        w.u32(d.junctions.len() as u32);
        w.u32(d.junction_heightmap_bytes.len() as u32);
        w.u32(d.unk68);
        w.u32(d.unk6c);
        Ok(())
    }
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// `ResourceBuilder.Build(NodeDictionary, 1)`: the dictionary as RSC7 bytes
/// (resource version 1), every array its own block.
pub fn serialize_node_dictionary(d: &NodeDictionary) -> Result<Vec<u8>> {
    if d.links.len() > u32::MAX as usize || d.nodes.len() > u32::MAX as usize {
        bail!("too many nodes or links for a 32-bit count");
    }
    let mut g = Graph::new();
    let pages = g.add(PagesInfo::default());
    fn array<T: Pod + 'static>(g: &mut Graph, items: &[T]) -> Option<BlockId> {
        if items.is_empty() { None } else { Some(g.add(StructArray { items: items.to_vec() })) }
    }
    let refs = array(&mut g, &d.junction_refs);
    let heightmap = array(&mut g, &d.junction_heightmap_bytes);
    let junctions = array(&mut g, &d.junctions);
    let links = array(&mut g, &d.links);
    let nodes = array(&mut g, &d.nodes);
    let root = g.add(DictionaryBlock { d: d.clone(), pages, nodes, links, junctions, heightmap, refs });
    g.build(root, pages, 1)
}

// ─── the file as the reference tool's XML lays it out ────────────────────────────────

/// Node speed, the two middle bits of `Flags5` (`YndNodeSpeed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSpeed { Slow = 0, Normal = 1, Fast = 2, Faster = 3 }

/// The top five bits of `Flags1` (`YndNodeSpecialType`); values the reference tool
/// does not name are `Other(n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSpecial {
    None,
    ParkingSpace,
    PedNodeRoadCrossing,
    PedNodeAssistedMovement,
    TrafficLightJunctionStop,
    StopSign,
    Caution,
    PedRoadCrossingNoWait,
    EmergencyVehiclesOnly,
    OffRoadJunction,
    Other(u8),
}

impl NodeSpecial {
    pub fn from_value(v: u8) -> Self {
        match v {
            0 => Self::None,
            2 => Self::ParkingSpace,
            10 => Self::PedNodeRoadCrossing,
            14 => Self::PedNodeAssistedMovement,
            15 => Self::TrafficLightJunctionStop,
            16 => Self::StopSign,
            17 => Self::Caution,
            18 => Self::PedRoadCrossingNoWait,
            19 => Self::EmergencyVehiclesOnly,
            20 => Self::OffRoadJunction,
            n => Self::Other(n),
        }
    }
    pub fn value(self) -> u8 {
        match self {
            Self::None => 0,
            Self::ParkingSpace => 2,
            Self::PedNodeRoadCrossing => 10,
            Self::PedNodeAssistedMovement => 14,
            Self::TrafficLightJunctionStop => 15,
            Self::StopSign => 16,
            Self::Caution => 17,
            Self::PedRoadCrossingNoWait => 18,
            Self::EmergencyVehiclesOnly => 19,
            Self::OffRoadJunction => 20,
            Self::Other(n) => n,
        }
    }
    /// `YndNode.IsSpecialTypeAPedNode`: the three crossing/walk types.
    pub fn is_ped(self) -> bool {
        matches!(self, Self::PedNodeRoadCrossing | Self::PedNodeAssistedMovement | Self::PedRoadCrossingNoWait)
    }
    /// The name the reference tool gives the type, `Other(n)` as `Unknown<n>`.
    pub fn name(self) -> String {
        match self {
            Self::None => "None".into(),
            Self::ParkingSpace => "ParkingSpace".into(),
            Self::PedNodeRoadCrossing => "PedNodeRoadCrossing".into(),
            Self::PedNodeAssistedMovement => "PedNodeAssistedMovement".into(),
            Self::TrafficLightJunctionStop => "TrafficLightJunctionStop".into(),
            Self::StopSign => "StopSign".into(),
            Self::Caution => "Caution".into(),
            Self::PedRoadCrossingNoWait => "PedRoadCrossingNoWait".into(),
            Self::EmergencyVehiclesOnly => "EmergencyVehiclesOnly".into(),
            Self::OffRoadJunction => "OffRoadJunction".into(),
            Self::Other(n) => format!("Unknown{n}"),
        }
    }
}

fn set_bit(byte: &mut u8, bit: u8, on: bool) {
    if on { *byte |= bit } else { *byte &= !bit }
}

/// A link leaving a node (`YndLink` over `NodeLink`): the node it leads to
/// and its flags. Lane counts and the rest are read out of the flag bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathLink {
    /// The cell and node the link leads to.
    pub area_id: u16,
    pub node_id: u16,
    pub flags0: u8,
    pub flags1: u8,
    pub flags2: u8,
    /// The link's length in metres, capped at 255.
    pub length: u8,
}

impl PathLink {
    /// A link to `(area_id, node_id)` with no flags.
    pub fn to(area_id: u16, node_id: u16) -> Self {
        Self { area_id, node_id, ..Default::default() }
    }
    pub fn lane_count_forward(&self) -> u8 { (self.flags2 >> 5) & 7 }
    pub fn set_lane_count_forward(&mut self, n: u8) { self.flags2 = (self.flags2 & !0xE0) | ((n & 7) << 5) }
    pub fn lane_count_backward(&self) -> u8 { (self.flags2 >> 2) & 7 }
    pub fn set_lane_count_backward(&mut self, n: u8) { self.flags2 = (self.flags2 & !0x1C) | ((n & 7) << 2) }
    /// The lane offset magnitude, bits 4..6 of `Flags1`.
    pub fn offset_value(&self) -> u8 { (self.flags1 >> 4) & 7 }
    pub fn negative_offset(&self) -> bool { self.flags1 >> 7 != 0 }
    /// `YndLink.LaneOffset`: `offset_value / 7`, negated by the sign bit, halved.
    pub fn lane_offset(&self) -> f32 {
        (self.offset_value() as f32 / 7.0) * if self.negative_offset() { -0.5 } else { 0.5 }
    }
    pub fn gps_both_ways(&self) -> bool { self.flags0 & 1 != 0 }
    pub fn narrow_road(&self) -> bool { self.flags1 & 2 != 0 }
    pub fn dont_use_for_navigation(&self) -> bool { self.flags2 & 1 != 0 }
    pub fn shortcut(&self) -> bool { self.flags2 & 2 != 0 }
    pub fn set_shortcut(&mut self, on: bool) { set_bit(&mut self.flags2, 2, on) }
    pub fn is_two_way(&self) -> bool { self.lane_count_forward() > 0 && self.lane_count_backward() > 0 }
    /// `YndLink.UpdateLength`: the distance between the two nodes, capped at 255.
    pub fn length_between(a: Vec3, b: Vec3) -> u8 {
        (b - a).length().min(255.0) as u8
    }
}

/// A node with its links (`YndNode`). The position is stored to a quarter
/// of a metre in x and y and a 32nd in z; [`PathNode::set_position`]
/// quantises the way the reference tool does.
#[derive(Debug, Clone, PartialEq)]
pub struct PathNode {
    pub area_id: u16,
    pub node_id: u16,
    /// A text (GXT) hash naming the street; 0 for none.
    pub street_name: u32,
    pub position: Vec3,
    pub flags0: u8,
    pub flags1: u8,
    pub flags2: u8,
    pub flags3: u8,
    pub flags4: u8,
    /// `Flags5`: the low three bits of the link count byte (speed in bits 1-2).
    pub flags5: u8,
    pub links: Vec<PathLink>,
}

impl Default for PathNode {
    fn default() -> Self {
        Self {
            area_id: 0, node_id: 0, street_name: 0, position: Vec3::new(0.0, 0.0, 0.0),
            flags0: 0, flags1: 0, flags2: 0, flags3: 0, flags4: 0, flags5: 0, links: Vec::new(),
        }
    }
}

impl PathNode {
    /// A node of cell `area_id` numbered `node_id` at `position`.
    pub fn new(area_id: u16, node_id: u16, position: Vec3) -> Self {
        let mut n = Self { area_id, node_id, ..Default::default() };
        n.set_position(position);
        n
    }

    /// `YndNode.SetPosition`: quantised to the stored precision (truncated
    /// towards zero, as the C# casts do).
    pub fn set_position(&mut self, p: Vec3) {
        let (x, y, z) = quantise(p);
        self.position = Vec3::new(x as f32 / 4.0, y as f32 / 4.0, z as f32 / 32.0);
    }

    pub fn speed(&self) -> NodeSpeed {
        match (self.flags5 >> 1) & 3 { 0 => NodeSpeed::Slow, 1 => NodeSpeed::Normal, 2 => NodeSpeed::Fast, _ => NodeSpeed::Faster }
    }
    pub fn set_speed(&mut self, s: NodeSpeed) { self.flags5 = (self.flags5 & !6) | (((s as u8) & 3) << 1) }

    // Flags0
    pub fn off_road(&self) -> bool { self.flags0 & 8 != 0 }
    pub fn set_off_road(&mut self, on: bool) { set_bit(&mut self.flags0, 8, on) }
    pub fn no_big_vehicles(&self) -> bool { self.flags0 & 32 != 0 }
    pub fn set_no_big_vehicles(&mut self, on: bool) { set_bit(&mut self.flags0, 32, on) }
    pub fn cannot_go_left(&self) -> bool { self.flags0 & 128 != 0 }
    pub fn set_cannot_go_left(&mut self, on: bool) { set_bit(&mut self.flags0, 128, on) }

    // Flags1
    pub fn slip_road(&self) -> bool { self.flags1 & 1 != 0 }
    pub fn set_slip_road(&mut self, on: bool) { set_bit(&mut self.flags1, 1, on) }
    pub fn indicate_keep_left(&self) -> bool { self.flags1 & 2 != 0 }
    pub fn set_indicate_keep_left(&mut self, on: bool) { set_bit(&mut self.flags1, 2, on) }
    pub fn indicate_keep_right(&self) -> bool { self.flags1 & 4 != 0 }
    pub fn set_indicate_keep_right(&mut self, on: bool) { set_bit(&mut self.flags1, 4, on) }
    pub fn left_turns_only(&self) -> bool { self.flags1 & 128 != 0 }
    pub fn set_left_turns_only(&mut self, on: bool) { set_bit(&mut self.flags1, 128, on) }
    /// `YndNode.Special`: the top five bits of `Flags1`.
    pub fn special(&self) -> NodeSpecial { NodeSpecial::from_value(self.flags1 >> 3) }
    pub fn set_special(&mut self, s: NodeSpecial) { self.flags1 = (self.flags1 & !0xF8) | (s.value() << 3) }
    /// `YndNode.IsPedNode`: a road crossing or assisted-movement node.
    pub fn is_ped_node(&self) -> bool { self.special().is_ped() }

    // Flags2
    pub fn no_gps(&self) -> bool { self.flags2 & 1 != 0 }
    pub fn set_no_gps(&mut self, on: bool) { set_bit(&mut self.flags2, 1, on) }
    pub fn is_junction(&self) -> bool { self.flags2 & 4 != 0 }
    pub fn set_is_junction(&mut self, on: bool) { set_bit(&mut self.flags2, 4, on) }
    pub fn is_disabled_unk1(&self) -> bool { self.flags2 & 16 != 0 }
    pub fn highway(&self) -> bool { self.flags2 & 64 != 0 }
    pub fn set_highway(&mut self, on: bool) { set_bit(&mut self.flags2, 64, on) }
    pub fn is_disabled_unk0(&self) -> bool { self.flags2 & 128 != 0 }
    /// Either of the two "disabled" bits the reference tool paints red.
    pub fn is_disabled(&self) -> bool { self.is_disabled_unk0() || self.is_disabled_unk1() }

    // Flags3
    pub fn tunnel(&self) -> bool { self.flags3 & 1 != 0 }
    pub fn set_tunnel(&mut self, on: bool) { set_bit(&mut self.flags3, 1, on) }
    /// The seven-bit path heuristic in `Flags3`.
    pub fn heuristic_value(&self) -> u8 { self.flags3 >> 1 }
    pub fn set_heuristic_value(&mut self, v: u8) { self.flags3 = (self.flags3 & !0xFE) | (v << 1) }

    // Flags4
    pub fn density(&self) -> u8 { self.flags4 & 15 }
    pub fn set_density(&mut self, d: u8) { self.flags4 = (self.flags4 & !15) | (d & 15) }
    pub fn dead_endness(&self) -> u8 { self.flags4 & 112 }
}

/// World position to the stored shorts, as `YndNode.SetPosition` casts.
fn quantise(p: Vec3) -> (i16, i16, i16) {
    ((p.x * 4.0) as i16, (p.y * 4.0) as i16, (p.z * 32.0) as i16)
}

/// A junction heightmap: `height` rows of `width` bytes, a row per 2 m
/// along y and a byte per 2 m along x, 0 at `min_z` and 255 at `max_z`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Heightmap {
    pub width: u8,
    pub height: u8,
    /// `width * height` bytes, row by row.
    pub values: Vec<u8>,
}

impl Heightmap {
    pub fn row(&self, y: usize) -> &[u8] {
        let w = self.width as usize;
        &self.values[y * w..(y + 1) * w]
    }
}

/// A junction (`YndJunction`): its footprint corner, height range and
/// heightmap, plus the ref that ties it to a node.
#[derive(Debug, Clone, PartialEq)]
pub struct PathJunction {
    /// The south-west corner of the heightmap in the world.
    pub position: Vec2,
    pub min_z: f32,
    pub max_z: f32,
    pub heightmap: Heightmap,
}

/// A path node cell as the reference tool's XML lays it out.
#[derive(Debug, Clone, PartialEq)]
pub struct Ynd {
    /// The root block's vtable value; kept so a rewritten file matches.
    pub vft: u32,
    pub vehicle_node_count: u32,
    pub ped_node_count: u32,
    pub nodes: Vec<PathNode>,
    pub junctions: Vec<PathJunction>,
    /// Which node each junction belongs to, by index into `junctions`.
    pub junction_refs: Vec<NodeJunctionRef>,
    /// The unnamed words of the root block, as read (see [`NodeDictionary`]).
    pub unknowns: [u32; 7],
}

impl Default for Ynd {
    fn default() -> Self {
        Self {
            vft: NODE_DICTIONARY_VFT, vehicle_node_count: 0, ped_node_count: 0,
            nodes: Vec::new(), junctions: Vec::new(), junction_refs: Vec::new(),
            unknowns: [0, 0, 1, 0, 0, 0, 0],
        }
    }
}

impl Ynd {
    /// An empty cell.
    pub fn new() -> Self { Self::default() }

    /// The cell's area id: what its nodes say, or `None` for an empty cell
    /// (the file name says then; see [`area_id_from_file_name`]).
    pub fn area_id(&self) -> Option<u32> {
        self.nodes.first().map(|n| n.area_id as u32)
    }

    /// The node numbered `node_id` in this cell.
    pub fn node(&self, node_id: u16) -> Option<&PathNode> {
        match self.nodes.get(node_id as usize) {
            Some(n) if n.node_id == node_id => Some(n),
            _ => self.nodes.iter().find(|n| n.node_id == node_id),
        }
    }

    /// The junction a node carries, through the refs.
    pub fn junction_of(&self, area_id: u16, node_id: u16) -> Option<&PathJunction> {
        let r = self.junction_refs.iter().find(|r| r.area_id == area_id && r.node_id == node_id)?;
        self.junctions.get(r.junction_id as usize)
    }

    /// `YndFile.AddNode`: a node at the next id, in the cell the others are in.
    pub fn add_node(&mut self, area_id: u16, position: Vec3) -> &mut PathNode {
        let node_id = self.nodes.len() as u16;
        self.nodes.push(PathNode::new(area_id, node_id, position));
        self.nodes.last_mut().unwrap()
    }

    /// `YndFile.RecalculateNodeIndices`: vehicle nodes first, ped nodes
    /// after, each group in its old order, renumbered from 0; the links
    /// inside this cell follow their nodes, and the vehicle and ped counts
    /// are set. Links into this cell from other cells cannot be fixed here.
    pub fn recalculate_node_indices(&mut self) {
        let area = self.area_id().unwrap_or(0) as u16;
        let mut order: Vec<usize> = (0..self.nodes.len()).filter(|&i| !self.nodes[i].is_ped_node()).collect();
        let vehicles = order.len();
        order.extend((0..self.nodes.len()).filter(|&i| self.nodes[i].is_ped_node()));
        let mut new_id = vec![0u16; self.nodes.len()];
        for (new, &old) in order.iter().enumerate() { new_id[old] = new as u16; }
        let by_old_id: std::collections::HashMap<u16, u16> =
            self.nodes.iter().enumerate().map(|(i, n)| (n.node_id, new_id[i])).collect();
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for &old in &order {
            let mut n = self.nodes[old].clone();
            n.node_id = new_id[old];
            for l in &mut n.links {
                if l.area_id == area {
                    if let Some(&id) = by_old_id.get(&l.node_id) { l.node_id = id; }
                }
            }
            nodes.push(n);
        }
        for r in &mut self.junction_refs {
            if r.area_id == area {
                if let Some(&id) = by_old_id.get(&r.node_id) { r.node_id = id; }
            }
        }
        self.nodes = nodes;
        self.vehicle_node_count = vehicles as u32;
        self.ped_node_count = (self.nodes.len() - vehicles) as u32;
    }

    /// `YndFile.InitNodesFromDictionary` and `YndJunctionHeightmap`: the
    /// nodes with their links, and the junctions with their heightmaps.
    pub fn from_dictionary(d: &NodeDictionary) -> Result<Self> {
        let mut nodes = Vec::with_capacity(d.nodes.len());
        for (i, n) in d.nodes.iter().enumerate() {
            let count = (n.link_count_flags >> 3) as usize;
            let start = n.link_id as usize;
            let links = d.links.get(start..start + count)
                .with_context(|| format!("node {i} names links {start}..{} of {}", start + count, d.links.len()))?;
            nodes.push(PathNode {
                area_id: n.area_id, node_id: n.node_id, street_name: n.street_name,
                position: Vec3::new(n.position_x as f32 / 4.0, n.position_y as f32 / 4.0, n.position_z as f32 / 32.0),
                flags0: n.flags0, flags1: n.flags1, flags2: n.flags2, flags3: n.flags3, flags4: n.flags4,
                flags5: n.link_count_flags & 7,
                links: links.iter().map(|l| PathLink {
                    area_id: l.area_id, node_id: l.node_id, flags0: l.flags0, flags1: l.flags1, flags2: l.flags2, length: l.link_length,
                }).collect(),
            });
        }
        let mut junctions = Vec::with_capacity(d.junctions.len());
        for (i, j) in d.junctions.iter().enumerate() {
            let count = j.heightmap_dim_x as usize * j.heightmap_dim_y as usize;
            let start = j.heightmap_ptr as usize;
            let values = d.junction_heightmap_bytes.get(start..start + count)
                .with_context(|| format!("junction {i} names heightmap bytes {start}..{} of {}", start + count, d.junction_heightmap_bytes.len()))?;
            junctions.push(PathJunction {
                position: Vec2::new(j.position_x as f32 / 4.0, j.position_y as f32 / 4.0),
                min_z: j.min_z as f32 / 32.0,
                max_z: j.max_z as f32 / 32.0,
                heightmap: Heightmap { width: j.heightmap_dim_x, height: j.heightmap_dim_y, values: values.to_vec() },
            });
        }
        Ok(Self {
            vft: d.vft,
            vehicle_node_count: d.nodes_count_vehicle,
            ped_node_count: d.nodes_count_ped,
            nodes, junctions,
            junction_refs: d.junction_refs.clone(),
            unknowns: [d.unk24, d.unk34, d.unk48, d.unk4c, d.unk5c, d.unk68, d.unk6c],
        })
    }

    /// `NodeDictionary.ReadXml`'s layout: links concatenated in node order,
    /// heightmaps in junction order, and the counts and offsets that follow.
    pub fn to_dictionary(&self) -> Result<NodeDictionary> {
        let mut nodes = Vec::with_capacity(self.nodes.len());
        let mut links = Vec::new();
        for n in &self.nodes {
            if n.links.len() > 31 {
                bail!("node {} has {} links; the count is stored in five bits, so 31 is the most", n.node_id, n.links.len());
            }
            let link_id = count_u16(links.len(), "links")?;
            for l in &n.links {
                links.push(NodeLink { area_id: l.area_id, node_id: l.node_id, flags0: l.flags0, flags1: l.flags1, flags2: l.flags2, link_length: l.length });
            }
            let (x, y, z) = quantise(n.position);
            nodes.push(Node {
                area_id: n.area_id, node_id: n.node_id, street_name: n.street_name, link_id,
                position_x: x, position_y: y, position_z: z,
                flags0: n.flags0, flags1: n.flags1, flags2: n.flags2, flags3: n.flags3, flags4: n.flags4,
                link_count_flags: ((n.links.len() as u8) << 3) | (n.flags5 & 7),
                ..Default::default()
            });
        }
        let mut junctions = Vec::with_capacity(self.junctions.len());
        let mut heightmap = Vec::new();
        for (i, j) in self.junctions.iter().enumerate() {
            let h = &j.heightmap;
            if h.values.len() != h.width as usize * h.height as usize {
                bail!("junction {i}: a {}x{} heightmap holds {} values, not {}", h.width, h.height, h.values.len(), h.width as usize * h.height as usize);
            }
            let heightmap_ptr = count_u16(heightmap.len(), "junction heightmap bytes")?;
            heightmap.extend_from_slice(&h.values);
            junctions.push(NodeJunction {
                max_z: (j.max_z * 32.0) as i16, min_z: (j.min_z * 32.0) as i16,
                position_x: (j.position.x * 4.0) as i16, position_y: (j.position.y * 4.0) as i16,
                heightmap_ptr, heightmap_dim_x: h.width, heightmap_dim_y: h.height,
            });
        }
        count_u16(self.junction_refs.len(), "junction refs")?;
        let [unk24, unk34, unk48, unk4c, unk5c, unk68, unk6c] = self.unknowns;
        Ok(NodeDictionary {
            vft: self.vft,
            nodes_count_vehicle: self.vehicle_node_count,
            nodes_count_ped: self.ped_node_count,
            unk24, unk34, unk48, unk4c, unk5c, unk68, unk6c,
            nodes, links, junctions, junction_heightmap_bytes: heightmap,
            junction_refs: self.junction_refs.clone(),
        })
    }
}

/// Parses a `.ynd` (RSC7 bytes, deflated or stored).
pub fn parse_ynd(data: &[u8]) -> Result<Ynd> {
    Ynd::from_dictionary(&parse_node_dictionary(data)?)
}

/// Serialises `ynd` to RSC7 bytes (resource version 1), as the reference tool's
/// `YndFile.Save` does for a file that came in as XML.
pub fn serialize_ynd(ynd: &Ynd) -> Result<Vec<u8>> {
    serialize_node_dictionary(&ynd.to_dictionary()?)
}

// ─── XML ─────────────────────────────────────────────────────────────────────

/// `YndXml.GetXml`: the `<NodeDictionary>` document. Street names print
/// through `names`, else as `hash_XXXXXXXX`.
pub fn ynd_to_xml(ynd: &Ynd, names: &NameTable) -> String {
    let mut x = XmlOut::new();
    x.open("NodeDictionary");
    x.value("VehicleNodeCount", ynd.vehicle_node_count);
    x.value("PedNodeCount", ynd.ped_node_count);
    write_items(&mut x, "Nodes", &ynd.nodes, |x, n| {
        x.value("AreaID", n.area_id);
        x.value("NodeID", n.node_id);
        x.string("StreetName", &hash_string(n.street_name, names));
        x.vec3("Position", n.position);
        x.value("Flags0", n.flags0);
        x.value("Flags1", n.flags1);
        x.value("Flags2", n.flags2);
        x.value("Flags3", n.flags3);
        x.value("Flags4", n.flags4);
        x.value("Flags5", n.flags5);
        write_items(x, "Links", &n.links, |x, l| {
            x.value("ToAreaID", l.area_id);
            x.value("ToNodeID", l.node_id);
            x.value("Flags0", l.flags0);
            x.value("Flags1", l.flags1);
            x.value("Flags2", l.flags2);
            x.value("LinkLength", l.length);
        });
    });
    write_items(&mut x, "Junctions", &ynd.junctions, |x, j| {
        x.self_closing(&format!("Position x=\"{}\" y=\"{}\"", crate::xml::float(j.position.x), crate::xml::float(j.position.y)));
        x.value("MinZ", crate::xml::float(j.min_z));
        x.value("MaxZ", crate::xml::float(j.max_z));
        x.value("SizeX", j.heightmap.width);
        x.value("SizeY", j.heightmap.height);
        x.raw_array("Heightmap", &j.heightmap.values, j.heightmap.width.max(1) as usize, |b| format!("{b:02X}"));
    });
    write_items(&mut x, "JunctionRefs", &ynd.junction_refs, |x, r| {
        x.value("AreaID", r.area_id);
        x.value("NodeID", r.node_id);
        x.value("JunctionID", r.junction_id);
        x.value("Unk0", r.unk0);
    });
    x.close("NodeDictionary");
    x.out
}

/// `XmlYnd.GetYnd`: a `<NodeDictionary>` document read back. A street name
/// that is not `hash_XXXXXXXX` is hashed as written (`GetTextHash`).
pub fn ynd_from_xml(xml: &str) -> Result<Ynd> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let root = doc.root_element();
    let tag = root.tag_name().name();
    if tag != "NodeDictionary" {
        bail!("the XML's root element is <{tag}>, not <NodeDictionary>: it is not a path node file");
    }
    let mut ynd = Ynd { vehicle_node_count: child_attr_u32(root, "VehicleNodeCount", "value"), ped_node_count: child_attr_u32(root, "PedNodeCount", "value"), ..Default::default() };
    for item in items(root, "Nodes") {
        let mut node = PathNode {
            area_id: child_attr_u32(item, "AreaID", "value") as u16,
            node_id: child_attr_u32(item, "NodeID", "value") as u16,
            street_name: parse_hash(&child_text(item, "StreetName")),
            flags0: child_attr_u32(item, "Flags0", "value") as u8,
            flags1: child_attr_u32(item, "Flags1", "value") as u8,
            flags2: child_attr_u32(item, "Flags2", "value") as u8,
            flags3: child_attr_u32(item, "Flags3", "value") as u8,
            flags4: child_attr_u32(item, "Flags4", "value") as u8,
            flags5: (child_attr_u32(item, "Flags5", "value") as u8) & 7,
            ..Default::default()
        };
        node.set_position(child_vec3(item, "Position"));
        node.links = items(item, "Links").into_iter().map(|l| PathLink {
            area_id: child_attr_u32(l, "ToAreaID", "value") as u16,
            node_id: child_attr_u32(l, "ToNodeID", "value") as u16,
            flags0: child_attr_u32(l, "Flags0", "value") as u8,
            flags1: child_attr_u32(l, "Flags1", "value") as u8,
            flags2: child_attr_u32(l, "Flags2", "value") as u8,
            length: child_attr_u32(l, "LinkLength", "value") as u8,
        }).collect();
        ynd.nodes.push(node);
    }
    for (i, item) in items(root, "Junctions").into_iter().enumerate() {
        let width = child_attr_u32(item, "SizeX", "value") as u8;
        let height = child_attr_u32(item, "SizeY", "value") as u8;
        let values = child(item, "Heightmap").map(hex_bytes).transpose().with_context(|| format!("junction {i} heightmap"))?.unwrap_or_default();
        if values.len() != width as usize * height as usize {
            bail!("junction {i}: a {width}x{height} heightmap needs {} values, the XML holds {}", width as usize * height as usize, values.len());
        }
        // Position and the heights are stored to a quarter and a 32nd; the
        // same casts as `NodeJunction.ReadXml` keep what a dump wrote.
        let px = (child_attr_f32(item, "Position", "x") * 4.0) as i16;
        let py = (child_attr_f32(item, "Position", "y") * 4.0) as i16;
        let min_z = (child_attr_f32(item, "MinZ", "value") * 32.0) as i16;
        let max_z = (child_attr_f32(item, "MaxZ", "value") * 32.0) as i16;
        ynd.junctions.push(PathJunction {
            position: Vec2::new(px as f32 / 4.0, py as f32 / 4.0),
            min_z: min_z as f32 / 32.0,
            max_z: max_z as f32 / 32.0,
            heightmap: Heightmap { width, height, values },
        });
    }
    for item in items(root, "JunctionRefs") {
        ynd.junction_refs.push(NodeJunctionRef {
            area_id: child_attr_u32(item, "AreaID", "value") as u16,
            node_id: child_attr_u32(item, "NodeID", "value") as u16,
            junction_id: child_attr_u32(item, "JunctionID", "value") as u16,
            unk0: child_attr_u32(item, "Unk0", "value") as u16,
        });
    }
    Ok(ynd)
}

/// `Xml.GetRawByteArray`: whitespace-separated hex bytes.
fn hex_bytes(n: roxmltree::Node) -> Result<Vec<u8>> {
    n.text().unwrap_or("").split_whitespace()
        .map(|w| u8::from_str_radix(w, 16).with_context(|| format!("'{w}' is not a hex byte")))
        .collect()
}

/// A `.ynd` as the reference tool's XML.
pub fn dump_ynd_xml(data: &[u8], names: &NameTable) -> Result<String> {
    Ok(ynd_to_xml(&parse_ynd(data)?, names))
}

/// The `.ynd` for a `<NodeDictionary>` document.
pub fn build_ynd_from_xml(xml: &str) -> Result<Vec<u8>> {
    serialize_ynd(&ynd_from_xml(xml)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_nodes() -> Ynd {
        let mut ynd = Ynd::new();
        let a = ynd.add_node(489, Vec3::new(-1200.25, 100.5, 30.125));
        a.street_name = 0x1234_5678;
        a.set_speed(NodeSpeed::Fast);
        a.set_highway(true);
        let mut link = PathLink::to(489, 1);
        link.set_lane_count_forward(2);
        link.set_lane_count_backward(1);
        link.length = 20;
        a.links.push(link);
        let b = ynd.add_node(489, Vec3::new(-1180.25, 100.5, 30.125));
        b.set_special(NodeSpecial::PedNodeRoadCrossing);
        let mut back = PathLink::to(489, 0);
        back.set_shortcut(true);
        b.links.push(back);
        b.links.push(PathLink::to(490, 7));
        ynd.junctions.push(PathJunction {
            position: Vec2::new(-1190.0, 96.0),
            min_z: 29.0,
            max_z: 31.0,
            heightmap: Heightmap { width: 3, height: 2, values: vec![0, 128, 255, 10, 20, 30] },
        });
        ynd.junction_refs.push(NodeJunctionRef { area_id: 489, node_id: 0, junction_id: 0, unk0: 0 });
        ynd.vehicle_node_count = 1;
        ynd.ped_node_count = 1;
        ynd
    }

    #[test]
    fn cell_maths_match_reference() {
        assert_eq!(cell_for_position(-1200.0, 100.0), (13, 16));
        assert_eq!(area_id(13, 16), 525);
        assert_eq!(cell_of_area(525), (13, 16));
        assert_eq!(cell_file_name(9, 15), "nodes489.ynd");
        assert_eq!(cell_bounds(0, 0), (-8192.0, -8192.0, -7680.0, -7680.0));
        assert_eq!(area_id_from_file_name("x64/levels/gta5/paths.rpf/Nodes489.ynd"), Some(489));
        assert_eq!(area_id_from_file_name("navmesh[108][96].ynv"), None);
        // Off the grid clamps to the edge cells.
        assert_eq!(cell_for_position(-9000.0, 9000.0), (0, 31));
        // Cayo Perico's cells carry the island flag over the cell they replace.
        assert!(is_island_area(1210) && !is_island_area(186));
        assert_eq!(cell_of_area(1210), (26, 5));
        assert!(same_cell(1210, 186) && !same_cell(1210, 187));
    }

    #[test]
    fn two_nodes_round_trip() {
        let ynd = two_nodes();
        let bytes = serialize_ynd(&ynd).unwrap();
        let back = parse_ynd(&bytes).unwrap();
        assert_eq!(back, ynd);
        assert_eq!(back.nodes[0].speed(), NodeSpeed::Fast);
        assert!(back.nodes[0].highway());
        assert_eq!(back.nodes[0].links[0].lane_count_forward(), 2);
        assert_eq!(back.nodes[0].links[0].lane_count_backward(), 1);
        assert!(back.nodes[1].is_ped_node());
        assert!(back.nodes[1].links[0].shortcut());
        assert_eq!(back.junction_of(489, 0).unwrap().heightmap.row(1), &[10, 20, 30]);
        assert_eq!(serialize_ynd(&back).unwrap(), bytes, "second serialisation must be byte-identical");

        let d = ynd.to_dictionary().unwrap();
        assert_eq!(d.nodes[1].link_id, 1);
        assert_eq!(d.nodes[1].link_count_flags >> 3, 2);
        assert_eq!(d.links.len(), 3);
        assert_eq!(d.junctions[0].heightmap_ptr, 0);
    }

    #[test]
    fn positions_are_quantised_as_reference_casts() {
        let mut n = PathNode::default();
        n.set_position(Vec3::new(10.3, -10.3, 5.01));
        assert_eq!(n.position, Vec3::new(10.25, -10.25, 5.0));
    }

    #[test]
    fn xml_round_trips_and_looks_like_references() {
        let ynd = two_nodes();
        let xml = ynd_to_xml(&ynd, &NameTable::core());
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<NodeDictionary>\n  <VehicleNodeCount value=\"1\" />\n  <PedNodeCount value=\"1\" />\n  <Nodes>\n    <Item>\n      <AreaID value=\"489\" />\n      <NodeID value=\"0\" />\n      <StreetName>hash_12345678</StreetName>\n      <Position x=\"-1200.25\" y=\"100.5\" z=\"30.125\" />\n"), "{xml}");
        assert!(xml.contains("      <Flags5 value=\"4\" />\n      <Links>\n        <Item>\n          <ToAreaID value=\"489\" />\n          <ToNodeID value=\"1\" />\n"), "{xml}");
        assert!(xml.contains("      <Position x=\"-1190\" y=\"96\" />\n      <MinZ value=\"29\" />\n      <MaxZ value=\"31\" />\n      <SizeX value=\"3\" />\n      <SizeY value=\"2\" />\n      <Heightmap>\n        00 80 FF\n        0A 14 1E\n      </Heightmap>\n"), "{xml}");
        assert!(xml.contains("  <JunctionRefs>\n    <Item>\n      <AreaID value=\"489\" />\n      <NodeID value=\"0\" />\n      <JunctionID value=\"0\" />\n      <Unk0 value=\"0\" />\n"), "{xml}");
        let back = ynd_from_xml(&xml).unwrap();
        assert_eq!(back, ynd);
        assert_eq!(build_ynd_from_xml(&xml).unwrap(), serialize_ynd(&ynd).unwrap());
    }

    #[test]
    fn a_street_name_in_the_xml_is_hashed_as_written() {
        let xml = "<NodeDictionary><Nodes><Item><StreetName>Vinewood Blvd</StreetName><Position x=\"1\" y=\"2\" z=\"3\" /></Item></Nodes></NodeDictionary>";
        let ynd = ynd_from_xml(xml).unwrap();
        assert_eq!(ynd.nodes[0].street_name, crate::rage_joaat("Vinewood Blvd"));
        assert_eq!(ynd.nodes[0].position, Vec3::new(1.0, 2.0, 3.0));
        assert!(ynd_from_xml("<Drawable />").unwrap_err().to_string().contains("not <NodeDictionary>"));
    }

    #[test]
    fn empty_cell_is_valid() {
        let bytes = serialize_ynd(&Ynd::new()).unwrap();
        let back = parse_ynd(&bytes).unwrap();
        assert!(back.nodes.is_empty() && back.junctions.is_empty());
        assert_eq!(back.area_id(), None);
    }

    #[test]
    fn recalculating_indices_puts_ped_nodes_last_and_moves_the_links() {
        let mut ynd = Ynd::new();
        let ped = ynd.add_node(489, Vec3::new(0.0, 0.0, 0.0));
        ped.set_special(NodeSpecial::PedRoadCrossingNoWait);
        ped.links.push(PathLink::to(489, 1));
        ynd.add_node(489, Vec3::new(4.0, 0.0, 0.0)).links.push(PathLink::to(489, 0));
        ynd.add_node(489, Vec3::new(8.0, 0.0, 0.0)).links.push(PathLink::to(488, 0));
        ynd.junction_refs.push(NodeJunctionRef { area_id: 489, node_id: 1, junction_id: 0, unk0: 0 });
        ynd.recalculate_node_indices();
        assert_eq!(ynd.nodes.iter().map(|n| n.position.x).collect::<Vec<_>>(), vec![4.0, 8.0, 0.0]);
        assert_eq!(ynd.nodes.iter().map(|n| n.node_id).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(ynd.nodes[0].links[0], PathLink::to(489, 2), "the vehicle node's link followed the ped node");
        assert_eq!(ynd.nodes[2].links[0], PathLink::to(489, 0));
        assert_eq!(ynd.nodes[1].links[0], PathLink::to(488, 0), "a link into another cell is left alone");
        assert_eq!(ynd.junction_refs[0].node_id, 0);
        assert_eq!((ynd.vehicle_node_count, ynd.ped_node_count), (2, 1));
    }

    #[test]
    fn too_many_links_on_a_node_is_an_error() {
        let mut ynd = Ynd::new();
        let n = ynd.add_node(1, Vec3::new(0.0, 0.0, 0.0));
        n.links = vec![PathLink::to(1, 0); 32];
        assert!(serialize_ynd(&ynd).unwrap_err().to_string().contains("31 is the most"));
    }
}
