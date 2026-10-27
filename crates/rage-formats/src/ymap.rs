//! Map placements (`.ymap`, RSC7 "Meta" format): the entities a map chunk
//! places — archetype, position, rotation, scale. This reads just the
//! `CMapData.entities` list, which is what's needed to put an MLO's
//! interior-local data (collision, rooms) into world space.
//!
//! Layout from the reference library `MetaTypes.cs` (`CMapData`, `CEntityDef`,
//! `CMloInstanceDef`); block walking shared with `ytyp.rs`.

use anyhow::{bail, Context, Result};

use crate::math::Vec3;
use crate::resource::{f32_le, prepare_rsc7, u16_le, u32_le, u64_le, vec3_le, ResReader};
use crate::ytyp::{decode_meta_pointer, read_meta_blocks, read_meta_u32_array, MetaBlock};

/// `MetaName` hashes (exact-case Jenkins of the structure name).
const HASH_CMAPDATA: u32 = 3_545_841_574;
const HASH_CENTITYDEF: u32 = 3_461_354_627;
const HASH_CMLOINSTANCEDEF: u32 = 164_374_718;

/// One placed entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YmapEntity {
    /// Lowercase JOAAT of the archetype name.
    pub archetype_hash: u32,
    pub flags: u32,
    pub guid: u32,
    pub position: Vec3,
    /// Quaternion as stored: x, y, z, w.
    pub rotation: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
    pub parent_index: i32,
    pub lod_dist: f32,
    /// How close the camera must be before this entity's LOD children are
    /// drawn in its place; negative means half of `lod_dist`
    /// (`YmapEntityDef.ChildLodDist`).
    pub child_lod_dist: f32,
    /// `rage__eLodType`: see the `LOD_*` constants.
    pub lod_level: u32,
    pub num_children: u32,
    /// True for a `CMloInstanceDef` (an interior placement).
    pub is_mlo_instance: bool,
}

impl YmapEntity {
    pub const LOD_HD: u32 = 0;
    pub const LOD_LOD: u32 = 1;
    pub const LOD_SLOD1: u32 = 2;
    pub const LOD_SLOD2: u32 = 3;
    pub const LOD_SLOD3: u32 = 4;
    pub const LOD_ORPHANHD: u32 = 5;
    pub const LOD_SLOD4: u32 = 6;

    /// The `LODTYPES_DEPTH_*` names, by level.
    pub const LOD_NAMES: [&'static str; 7] = ["hd", "lod", "slod1", "slod2", "slod3", "orphanhd", "slod4"];

    /// `flags` bit 3: the entity's LOD parent lives in the parent `.ymap`,
    /// not this one (`YmapEntityDef.LodInParentYmap`).
    pub fn lod_in_parent_ymap(&self) -> bool {
        self.flags & (1 << 3) != 0
    }

    /// Transforms an entity-local point into world space (scale, then the
    /// stored rotation, then translation). Map entities store the *inverse*
    /// rotation, so the conjugate is applied — matching the reference tool's
    /// `YmapEntityDef.SetOrientation` (`Orientation = Quaternion.Invert(rotation)`).
    pub fn to_world(&self, local: Vec3) -> Vec3 {
        let scaled = Vec3::new(local.x * self.scale_xy, local.y * self.scale_xy, local.z * self.scale_z);
        let [x, y, z, w] = self.rotation;
        // conjugate: rotate by (-x, -y, -z, w)
        rotate(scaled, [-x, -y, -z, w]) + self.position
    }
}

/// Rotates `v` by the unit quaternion `q` (x, y, z, w).
pub fn rotate(v: Vec3, q: [f32; 4]) -> Vec3 {
    let u = Vec3::new(q[0], q[1], q[2]);
    let s = q[3];
    // v' = 2 (u·v) u + (s² − u·u) v + 2 s (u × v)
    u * (2.0 * u.dot(v)) + v * (s * s - u.dot(u)) + u.cross(v) * (2.0 * s)
}

/// An interior placement: where a `.ymap` puts an MLO, which of its entity
/// sets start switched on, and which floor and room group it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct MloInstance {
    /// The placement itself (`is_mlo_instance` is always true).
    pub entity: YmapEntity,
    pub group_id: u32,
    pub floor_id: u32,
    /// Name hashes of the `CMloEntitySet`s this placement enables.
    pub default_entity_sets: Vec<u32>,
    pub num_exit_portals: u32,
}

/// The `CMapData` fields that describe the map itself: its name, the parent
/// map it is a LOD child of, its flags and the boxes it streams within.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YmapHeader {
    /// Lowercase JOAAT of the map's name (the reference tool shows it as the ymap's
    /// internal name).
    pub name_hash: u32,
    /// 0 when the map has no parent.
    pub parent_hash: u32,
    pub flags: u32,
    pub content_flags: u32,
    pub streaming_extents_min: Vec3,
    pub streaming_extents_max: Vec3,
    pub entities_extents_min: Vec3,
    pub entities_extents_max: Vec3,
}

impl YmapHeader {
    /// `flags` bit 0: the map is a script-requested one (`SCRIPTED`).
    pub const FLAG_SCRIPTED: u32 = 1;
    /// `flags` bit 1: the map's entities are LOD children (`LOD`).
    pub const FLAG_LOD: u32 = 2;

    /// The `contentFlags` bits by name, in bit order.
    pub const CONTENT_FLAG_NAMES: [&'static str; 11] = [
        "HD", "LOD", "SLOD2+", "Interior", "SLOD", "Occlusion", "Physics", "LOD lights", "Distant lights", "Critical", "Grass",
    ];

    /// The names of the set `contentFlags` bits.
    pub fn content_flag_names(&self) -> Vec<&'static str> {
        Self::CONTENT_FLAG_NAMES.iter().enumerate().filter(|(i, _)| self.content_flags & (1 << i) != 0).map(|(_, n)| *n).collect()
    }
}

/// Everything this module reads from a `.ymap`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ymap {
    pub header: YmapHeader,
    pub entities: Vec<YmapEntity>,
    pub mlo_instances: Vec<MloInstance>,
}

/// Reads the `CMapData` header fields (name @8, parent @12, flags @16,
/// contentFlags @20, the two extents boxes @32..96).
pub fn parse_ymap_header(data: &[u8]) -> Result<YmapHeader> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    let blocks = read_meta_blocks(&reader).context("ymap")?;
    let map = blocks.iter().find(|b| b.name_hash == HASH_CMAPDATA).context("ymap: CMapData block not found")?;
    let d = &map.data;
    if d.len() < 96 {
        bail!("ymap: CMapData block too small");
    }
    Ok(YmapHeader {
        name_hash: u32_le(d, 8),
        parent_hash: u32_le(d, 12),
        flags: u32_le(d, 16),
        content_flags: u32_le(d, 20),
        streaming_extents_min: vec3_le(d, 32),
        streaming_extents_max: vec3_le(d, 48),
        entities_extents_min: vec3_le(d, 64),
        entities_extents_max: vec3_le(d, 80),
    })
}

/// Gives the map a new `CMapData.name` (and, when `parent` is given, a new
/// `parent`), by the fixed layout rather than the schema, so it works on
/// any `.ymap` the header reader accepts. Returns the file re-paged into a
/// fresh RSC7 container of the same version, with how many fields changed.
pub fn set_map_name(data: &[u8], name_hash: u32, parent: Option<u32>) -> Result<(Vec<u8>, usize)> {
    let (mut system, graphics) = prepare_rsc7(data)?;
    let version = crate::resource::resource_version_from_flags(u32_le(data, 8), u32_le(data, 12));
    let offset = {
        let reader = ResReader { system: &system, graphics: &graphics };
        let blocks = read_meta_blocks(&reader).context("ymap")?;
        let map = blocks.iter().find(|b| b.name_hash == HASH_CMAPDATA).context("ymap: CMapData block not found")?;
        if map.data.len() < 96 {
            bail!("ymap: CMapData block too small");
        }
        map.system_offset.context("ymap: CMapData block is not in the system section")?
    };
    let mut changed = 0;
    if u32_le(&system, offset + 8) != name_hash {
        system[offset + 8..offset + 12].copy_from_slice(&name_hash.to_le_bytes());
        changed += 1;
    }
    if let Some(p) = parent.filter(|&p| u32_le(&system, offset + 12) != p) {
        system[offset + 12..offset + 16].copy_from_slice(&p.to_le_bytes());
        changed += 1;
    }
    Ok((crate::resource::build_rsc7(version, &system, &graphics), changed))
}

/// Header, entities and interior placements in one go.
pub fn parse_ymap(data: &[u8]) -> Result<Ymap> {
    Ok(Ymap { header: parse_ymap_header(data)?, entities: parse_ymap_entities(data)?, mlo_instances: parse_ymap_mlo_instances(data)? })
}

/// Walks a `.ymap`'s Meta blocks and reads `CMapData.entities` as a list of
/// packed pointers, which the two entity readers below then decode.
fn map_entity_pointers(data: &[u8]) -> Result<(Vec<MetaBlock>, Vec<u64>)> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    let blocks = read_meta_blocks(&reader).context("ymap")?;
    let map = blocks.iter().find(|b| b.name_hash == HASH_CMAPDATA).context("ymap: CMapData block not found")?;
    const ENTITIES_OFFSET: usize = 96;
    if map.data.len() < ENTITIES_OFFSET + 16 {
        bail!("ymap: CMapData block too small");
    }
    let ptr = u64_le(&map.data, ENTITIES_OFFSET);
    let count = u16_le(&map.data, ENTITIES_OFFSET + 8) as usize;
    let Some((arr_block, arr_off)) = decode_meta_pointer(ptr) else { return Ok((blocks, Vec::new())) };
    let ptrs = {
        let arr = blocks.get(arr_block).context("ymap: entities pointer block out of range")?;
        let bytes = arr.data.get(arr_off..arr_off + count * 8).context("ymap: entities pointer array out of bounds")?;
        (0..count).map(|i| u64_le(bytes, i * 8)).collect()
    };
    Ok((blocks, ptrs))
}

/// Parses the entity list of a `.ymap`.
pub fn parse_ymap_entities(data: &[u8]) -> Result<Vec<YmapEntity>> {
    let (blocks, ptrs) = map_entity_pointers(data)?;
    let mut out = Vec::with_capacity(ptrs.len());
    for ptr in ptrs {
        let Some((bi, off)) = decode_meta_pointer(ptr) else { continue };
        let Some(block) = blocks.get(bi) else { continue };
        let is_mlo_instance = match block.name_hash {
            HASH_CENTITYDEF => false,
            HASH_CMLOINSTANCEDEF => true,
            _ => continue,
        };
        let Some(e) = block.data.get(off..off + 128) else { continue };
        out.push(read_entity(e, is_mlo_instance));
    }
    Ok(out)
}

/// Parses just the interior placements of a `.ymap` — the entities
/// `parse_ymap_entities` flags as MLO instances, with the extra
/// `CMloInstanceDef` fields that `YmapEntity` has no room for.
pub fn parse_ymap_mlo_instances(data: &[u8]) -> Result<Vec<MloInstance>> {
    let (blocks, ptrs) = map_entity_pointers(data)?;
    let mut out = Vec::new();
    for ptr in ptrs {
        let Some((bi, off)) = decode_meta_pointer(ptr) else { continue };
        let Some(block) = blocks.get(bi) else { continue };
        if block.name_hash != HASH_CMLOINSTANCEDEF {
            continue;
        }
        // A CMloInstanceDef is a 128-byte CEntityDef plus 32 bytes of its own.
        let Some(e) = block.data.get(off..off + 160) else { continue };
        out.push(MloInstance {
            entity: read_entity(e, true),
            group_id: u32_le(e, 128),
            floor_id: u32_le(e, 132),
            default_entity_sets: read_meta_u32_array(&blocks, e, 136),
            num_exit_portals: u32_le(e, 152),
        });
    }
    Ok(out)
}

/// Decodes a `CEntityDef` (the first 128 bytes of a `CMloInstanceDef` too).
pub(crate) fn read_entity(e: &[u8], is_mlo_instance: bool) -> YmapEntity {
    YmapEntity {
        archetype_hash: u32_le(e, 8),
        flags: u32_le(e, 12),
        guid: u32_le(e, 16),
        position: vec3_le(e, 32),
        rotation: [f32_le(e, 48), f32_le(e, 52), f32_le(e, 56), f32_le(e, 60)],
        scale_xy: f32_le(e, 64),
        scale_z: f32_le(e, 68),
        parent_index: u32_le(e, 72) as i32,
        lod_dist: f32_le(e, 76),
        child_lod_dist: f32_le(e, 80),
        lod_level: u32_le(e, 84),
        num_children: u32_le(e, 88),
        is_mlo_instance,
    }
}

/// Byte-built `.ymap` fixtures. The exterior one is shared with downstream
/// crates' tests through the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    use super::*;
    use crate::hash::rage_joaat;
    use crate::resource::{build_rsc7, SYSTEM_BASE};

    /// A `.ymap` called `name` placing plain (non-MLO) entities: one
    /// `CEntityDef` per `(archetype name, position, yaw degrees)`, flags 32
    /// (static), lod distance 100. The map's extents are the entities'.
    pub fn sample_exterior_ymap(name: &str, entities: &[(&str, Vec3, f32)]) -> Vec<u8> {
        let entities: Vec<SampleEntity> = entities
            .iter()
            .map(|(archetype, position, yaw)| SampleEntity { archetype, position: *position, yaw: *yaw, ..SampleEntity::default() })
            .collect();
        sample_lod_ymap(name, None, &entities)
    }

    /// One entity of [`sample_lod_ymap`]: the plain fixture's archetype,
    /// position and heading, plus the LOD fields a hierarchy needs.
    #[derive(Debug, Clone, Copy)]
    pub struct SampleEntity<'a> {
        pub archetype: &'a str,
        pub position: Vec3,
        pub yaw: f32,
        /// `flags`; 32 (static) by default, `| 8` for a parent in the parent map.
        pub flags: u32,
        pub parent_index: i32,
        pub lod_dist: f32,
        pub child_lod_dist: f32,
        pub lod_level: u32,
        pub num_children: u32,
    }

    impl Default for SampleEntity<'_> {
        fn default() -> Self {
            SampleEntity {
                archetype: "",
                position: Vec3::new(0.0, 0.0, 0.0),
                yaw: 0.0,
                flags: 32,
                parent_index: -1,
                lod_dist: 100.0,
                child_lod_dist: -1.0,
                lod_level: YmapEntity::LOD_HD,
                num_children: 0,
            }
        }
    }

    /// A `.ymap` called `name`, the LOD child of `parent` when given, placing
    /// `entities` with every LOD field as asked. The map's extents are the
    /// entities'.
    pub fn sample_lod_ymap(name: &str, parent: Option<&str>, entities: &[SampleEntity]) -> Vec<u8> {
        let count = entities.len();
        let entity_bytes = count * 128;
        let ptr_bytes = (count * 8).max(8);
        // Blocks: CMapData at 0x0B0 (368), pointer array, then the entities.
        let ptr_off = 0x220;
        let ent_off = (ptr_off + ptr_bytes + 15) & !15;
        let mut sys = vec![0u8; (ent_off + entity_bytes + 0xFF) & !0xFF];

        let blocks: [(u32, usize, usize); 3] = [(HASH_CMAPDATA, 0x0B0, 368), (0, ptr_off, ptr_bytes), (HASH_CENTITYDEF, ent_off, entity_bytes.max(16))];
        put_u32(&mut sys, 0x1C, 1); // root block
        put_u64(&mut sys, 0x30, SYSTEM_BASE + 0x70);
        put_u16(&mut sys, 0x4C, blocks.len() as u16);
        for (i, &(hash, off, len)) in blocks.iter().enumerate() {
            let h = 0x70 + i * 16;
            put_u32(&mut sys, h, hash);
            put_u32(&mut sys, h + 4, len as u32);
            put_u64(&mut sys, h + 8, SYSTEM_BASE + off as u64);
        }

        let m = blocks[0].1;
        put_u32(&mut sys, m + 8, rage_joaat(name));
        if let Some(parent) = parent {
            put_u32(&mut sys, m + 12, rage_joaat(parent));
            put_u32(&mut sys, m + 16, YmapHeader::FLAG_LOD);
        }
        put_u32(&mut sys, m + 20, 1); // contentFlags: HD
        let (mut lo, mut hi) = (Vec3::new(f32::MAX, f32::MAX, f32::MAX), Vec3::new(f32::MIN, f32::MIN, f32::MIN));
        for e in entities {
            let p = e.position;
            lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        put_vec3(&mut sys, m + 32, lo);
        put_vec3(&mut sys, m + 48, hi);
        put_vec3(&mut sys, m + 64, lo);
        put_vec3(&mut sys, m + 80, hi);
        if count > 0 {
            put_array(&mut sys, m + 96, 1, count as u16);
        }
        for (n, ent) in entities.iter().enumerate() {
            put_u64(&mut sys, ptr_off + n * 8, 3 | ((n * 128) as u64) << 12);
            let e = ent_off + n * 128;
            put_u32(&mut sys, e + 8, rage_joaat(&ent.archetype.to_lowercase()));
            put_u32(&mut sys, e + 12, ent.flags);
            put_vec3(&mut sys, e + 32, ent.position);
            // Stored as the inverse: a heading of +yaw is stored as -yaw.
            let half = -ent.yaw.to_radians() / 2.0;
            put_f32(&mut sys, e + 56, half.sin());
            put_f32(&mut sys, e + 60, half.cos());
            put_f32(&mut sys, e + 64, 1.0);
            put_f32(&mut sys, e + 68, 1.0);
            put_u32(&mut sys, e + 72, ent.parent_index as u32);
            put_f32(&mut sys, e + 76, ent.lod_dist);
            put_f32(&mut sys, e + 80, ent.child_lod_dist);
            put_u32(&mut sys, e + 84, ent.lod_level);
            put_u32(&mut sys, e + 88, ent.num_children);
        }
        build_rsc7(2, &sys, &[])
    }

    #[test]
    fn the_lod_fixture_writes_the_hierarchy_fields() {
        let lod = SampleEntity { archetype: "slod_block", lod_dist: 800.0, child_lod_dist: 300.0, lod_level: YmapEntity::LOD_LOD, num_children: 1, ..SampleEntity::default() };
        let hd = SampleEntity { archetype: "prop_a", position: Vec3::new(1.0, 2.0, 3.0), parent_index: 0, flags: 32 | 8, ..SampleEntity::default() };
        let ymap = parse_ymap(&sample_lod_ymap("child_map", Some("parent_map"), &[lod, hd])).unwrap();
        assert_eq!(ymap.header.parent_hash, rage_joaat("parent_map"));
        assert_eq!(ymap.header.flags, YmapHeader::FLAG_LOD);
        let [a, b] = ymap.entities.as_slice() else { panic!("two entities") };
        assert_eq!((a.lod_dist, a.child_lod_dist, a.lod_level, a.num_children), (800.0, 300.0, YmapEntity::LOD_LOD, 1));
        assert_eq!((b.parent_index, b.lod_level, b.child_lod_dist), (0, YmapEntity::LOD_HD, -1.0));
        assert!(b.lod_in_parent_ymap() && !a.lod_in_parent_ymap());
    }

    #[test]
    fn the_map_name_is_set_by_layout() {
        let data = sample_exterior_ymap("map1", &[("prop_a", Vec3::new(10.0, 20.0, 30.0), 90.0)]);
        let (renamed, changed) = set_map_name(&data, rage_joaat("casas_praia_extras"), Some(rage_joaat("parent_map"))).unwrap();
        assert_eq!(changed, 2);
        let ymap = parse_ymap(&renamed).unwrap();
        assert_eq!(ymap.header.name_hash, rage_joaat("casas_praia_extras"));
        assert_eq!(ymap.header.parent_hash, rage_joaat("parent_map"));
        assert_eq!(ymap.entities.len(), 1);
        assert_eq!(ymap.entities[0].position, Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(set_map_name(&renamed, rage_joaat("casas_praia_extras"), None).unwrap().1, 0);
    }

    #[test]
    fn the_exterior_fixture_round_trips() {
        let data = sample_exterior_ymap("paleto_props", &[("prop_a", Vec3::new(10.0, 20.0, 30.0), 90.0), ("prop_b", Vec3::new(-5.0, 2.0, 1.0), 0.0)]);
        let ymap = parse_ymap(&data).unwrap();
        assert_eq!(ymap.header.name_hash, rage_joaat("paleto_props"));
        assert_eq!(ymap.header.entities_extents_min, Vec3::new(-5.0, 2.0, 1.0));
        assert_eq!(ymap.entities.len(), 2);
        assert_eq!(ymap.entities[0].archetype_hash, rage_joaat("prop_a"));
        assert_eq!(ymap.entities[1].position, Vec3::new(-5.0, 2.0, 1.0));
        assert!(ymap.mlo_instances.is_empty());
        // +90° heading: local +x becomes world +y.
        let p = ymap.entities[0].to_world(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(10.0, 21.0, 30.0)).length() < 1e-4, "{p:?}");
        assert_eq!(parse_ymap(&sample_exterior_ymap("empty", &[])).unwrap().entities.len(), 0);
    }


    #[test]
    fn rotation_by_quarter_turn_about_z() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let r = rotate(Vec3::new(1.0, 0.0, 0.0), [0.0, 0.0, h, h]);
        assert!((r - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-5, "{r:?}");
    }

    #[test]
    fn to_world_applies_conjugate_then_translation() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let e = YmapEntity {
            archetype_hash: 0, flags: 0, guid: 0, position: Vec3::new(10.0, 20.0, 30.0),
            rotation: [0.0, 0.0, h, h], scale_xy: 1.0, scale_z: 1.0, parent_index: -1, lod_dist: 0.0,
            child_lod_dist: -1.0, lod_level: 0, num_children: 0, is_mlo_instance: true,
        };
        // Stored quaternion is the inverse, so a +90° store rotates -90°.
        let p = e.to_world(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(10.0, 19.0, 30.0)).length() < 1e-5, "{p:?}");
    }

    // ─── MLO instance fixture ─────────────────────────────────────────────────

    fn put_u16(s: &mut [u8], off: usize, v: u16) { s[off..off + 2].copy_from_slice(&v.to_le_bytes()); }
    fn put_u32(s: &mut [u8], off: usize, v: u32) { s[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
    fn put_u64(s: &mut [u8], off: usize, v: u64) { s[off..off + 8].copy_from_slice(&v.to_le_bytes()); }
    fn put_f32(s: &mut [u8], off: usize, v: f32) { s[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
    fn put_vec3(s: &mut [u8], off: usize, v: Vec3) {
        put_f32(s, off, v.x);
        put_f32(s, off + 4, v.y);
        put_f32(s, off + 8, v.z);
    }
    /// A Meta array field: packed pointer at `off`, `count1` eight bytes later.
    fn put_array(s: &mut [u8], off: usize, block_index: usize, count: u16) {
        put_u64(s, off, block_index as u64 + 1);
        put_u16(s, off + 8, count);
    }

    /// `(structure-name hash, offset, length)` of each data block.
    const YMAP_BLOCKS: [(u32, usize, usize); 4] = [
        (HASH_CMAPDATA, 0x0B0, 368),        // 0: CMapData
        (0, 0x220, 8),                      // 1: entities pointer array
        (HASH_CMLOINSTANCEDEF, 0x230, 160), // 2: the one CMloInstanceDef
        (0, 0x2D0, 8),                      // 3: u32 array: two entity-set names
    ];

    const INSTANCE_POSITION: Vec3 = Vec3 { x: 100.0, y: 200.0, z: 30.0 };

    /// A hand-built `.ymap` placing one interior with two default entity sets
    /// switched on.
    fn minimal_mlo_ymap(instance_block_hash: u32) -> Vec<u8> {
        build_rsc7(2, &mlo_ymap_system(instance_block_hash), &[])
    }

    /// [`minimal_mlo_ymap`]'s Meta system section, before it is wrapped.
    fn mlo_ymap_system(instance_block_hash: u32) -> Vec<u8> {
        let mut sys = vec![0u8; 0x300];

        put_u64(&mut sys, 0x30, SYSTEM_BASE + 0x70); // DataBlocksPointer
        put_u16(&mut sys, 0x4C, YMAP_BLOCKS.len() as u16); // DataBlocksCount
        for (i, &(hash, off, len)) in YMAP_BLOCKS.iter().enumerate() {
            let h = 0x70 + i * 16;
            put_u32(&mut sys, h, if i == 2 { instance_block_hash } else { hash });
            put_u32(&mut sys, h + 4, len as u32);
            put_u64(&mut sys, h + 8, SYSTEM_BASE + off as u64);
        }

        // CMapData.entities -> block 1, one entry, pointing at block 2.
        put_array(&mut sys, YMAP_BLOCKS[0].1 + 96, 1, 1);
        put_u64(&mut sys, YMAP_BLOCKS[1].1, 3);

        // CMloInstanceDef: a CEntityDef, then the interior-only fields.
        let e = YMAP_BLOCKS[2].1;
        put_u32(&mut sys, e + 8, rage_joaat("v_test_mlo")); // archetypeName
        put_vec3(&mut sys, e + 32, INSTANCE_POSITION);
        put_f32(&mut sys, e + 60, 1.0); // rotation w
        put_f32(&mut sys, e + 64, 1.0); // scaleXY
        put_f32(&mut sys, e + 68, 1.0); // scaleZ
        put_u32(&mut sys, e + 72, -1i32 as u32); // parentIndex
        put_u32(&mut sys, e + 128, 5); // groupId
        put_u32(&mut sys, e + 132, 2); // floorId
        put_array(&mut sys, e + 136, 3, 2); // defaultEntitySets
        put_u32(&mut sys, e + 152, 3); // numExitPortals

        let u = YMAP_BLOCKS[3].1;
        put_u32(&mut sys, u, rage_joaat("set_kitchen"));
        put_u32(&mut sys, u + 4, rage_joaat("set_hall"));

        sys
    }

    #[test]
    fn parses_the_map_header() {
        let mut sys = mlo_ymap_system(HASH_CMLOINSTANCEDEF);
        let m = YMAP_BLOCKS[0].1;
        put_u32(&mut sys, m + 8, rage_joaat("map1"));
        put_u32(&mut sys, m + 16, 1);
        put_u32(&mut sys, m + 20, 0b1001);
        put_vec3(&mut sys, m + 32, Vec3::new(-10.0, -20.0, -30.0));
        put_vec3(&mut sys, m + 48, Vec3::new(10.0, 20.0, 30.0));
        put_vec3(&mut sys, m + 64, Vec3::new(-1.0, -2.0, -3.0));
        put_vec3(&mut sys, m + 80, Vec3::new(1.0, 2.0, 3.0));
        let ymap = parse_ymap(&build_rsc7(2, &sys, &[])).expect("should parse");
        assert_eq!(ymap.header.name_hash, rage_joaat("map1"));
        assert_eq!(ymap.header.parent_hash, 0);
        assert_eq!(ymap.header.flags, YmapHeader::FLAG_SCRIPTED);
        assert_eq!(ymap.header.content_flag_names(), ["HD", "Interior"]);
        assert_eq!(ymap.header.streaming_extents_max, Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(ymap.header.entities_extents_min, Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(ymap.entities.len(), 1);
        assert_eq!(ymap.mlo_instances.len(), 1);
    }

    #[test]
    fn parses_mlo_instance_with_default_entity_sets() {
        let instances = parse_ymap_mlo_instances(&minimal_mlo_ymap(HASH_CMLOINSTANCEDEF))
            .expect("fixture should parse");
        assert_eq!(instances.len(), 1);
        let i = &instances[0];
        assert_eq!(i.entity.archetype_hash, rage_joaat("v_test_mlo"));
        assert_eq!(i.entity.position, INSTANCE_POSITION);
        assert!(i.entity.is_mlo_instance);
        assert_eq!(i.group_id, 5);
        assert_eq!(i.floor_id, 2);
        assert_eq!(i.default_entity_sets, vec![rage_joaat("set_kitchen"), rage_joaat("set_hall")]);
        assert_eq!(i.num_exit_portals, 3);
    }

    #[test]
    fn plain_entities_are_not_mlo_instances() {
        let ymap = minimal_mlo_ymap(HASH_CENTITYDEF);
        assert!(parse_ymap_mlo_instances(&ymap).expect("should parse").is_empty());
        // The same file still yields the placement through the entity list.
        let entities = parse_ymap_entities(&ymap).expect("should parse");
        assert_eq!(entities.len(), 1);
        assert!(!entities[0].is_mlo_instance);
    }

    #[test]
    fn mlo_instance_without_default_entity_sets_yields_an_empty_list() {
        let mut sys = mlo_ymap_system(HASH_CMLOINSTANCEDEF);
        put_u64(&mut sys, YMAP_BLOCKS[2].1 + 136, 0); // defaultEntitySets: null
        put_u16(&mut sys, YMAP_BLOCKS[2].1 + 144, 0);
        let instances = parse_ymap_mlo_instances(&build_rsc7(2, &sys, &[])).expect("should parse");
        assert_eq!(instances.len(), 1);
        assert!(instances[0].default_entity_sets.is_empty());
    }
}
