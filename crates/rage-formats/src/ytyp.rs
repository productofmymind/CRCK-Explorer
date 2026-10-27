// Parses just enough of a .ytyp (RSC7-wrapped "Meta" format) to answer one
// question: which texture dictionary does a given archetype (prop/model)
// reference? That is `CBaseArchetypeDef.textureDictionary`, keyed by the
// archetype's `name` (or `assetName` when `name` is 0), matching how the
// game's own renderer picks a texture dictionary for a drawable
// (the reference tool `Archetype.cs` / `Renderer.cs TryGetRenderable`).
//
// A .ytyp's Meta container is a third format alongside the fixed-pointer-
// chasing RSC7 resources (Drawable, Texture, ...) and the PSO/RBF text
// format: a self-describing binary blob of typed "data blocks", each
// tagged with a structure-name hash. We only care about two structure
// types, `CMapTypes` (the root, holding the archetype list) and
// `CBaseArchetypeDef`/`CTimeArchetypeDef`/`CMloArchetypeDef` (one per
// archetype; all three share the same 144-byte layout for the fields we
// read, differing only in what follows).
//
// Byte layout ported from the reference library (Meta.cs, MetaTypes.cs,
// YtypFile.cs) — see that project for the full, generic Meta/PSO reader
// this deliberately does not reimplement.

use anyhow::{bail, Context, Result};
use crate::math::Vec3;
use crate::resource::{f32_le, prepare_rsc7, u16_le, u32_le, u64_le, vec3_le, ResReader, SYSTEM_BASE};
use crate::ymap::{read_entity, YmapEntity};

/// Structure-name hashes as the reference tool's `MetaName` enum defines them: the
/// RAGE Jenkins hash of the exact-case structure name (unlike texture/file
/// names elsewhere in this crate, these are *not* lowercased first).
const HASH_CMAPTYPES: u32 = 3_649_811_809;
const HASH_CBASE_ARCHETYPE_DEF: u32 = 2_195_127_427;
const HASH_CTIME_ARCHETYPE_DEF: u32 = 1_991_296_364;
const HASH_CMLO_ARCHETYPE_DEF: u32 = 273_704_021;
const HASH_CENTITYDEF: u32 = 3_461_354_627;
// The structure names an MLO's own blocks carry. A block reached through an
// `Array_Structure` (rooms, portals, entity sets) or a `CharPointer` (a room
// name, into a `STRING` block) is read by pointer and stride, so the parser
// never has to match on these; they are kept as the documented, test-asserted
// record of what those blocks are, and are what the fixtures tag them with.
#[allow(dead_code)]
const HASH_CMLO_ROOM_DEF: u32 = 186_126_833;
#[allow(dead_code)]
const HASH_CMLO_PORTAL_DEF: u32 = 2_572_186_314;
#[allow(dead_code)]
const HASH_CMLO_ENTITY_SET: u32 = 3_601_308_153;
#[allow(dead_code)]
const HASH_STRING: u32 = 3_358_528_679;

/// One archetype's texture-dictionary binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchetypeTxd {
    /// `name` (or `assetName` when `name` is 0) — the archetype's own
    /// lowercase-JOAAT name hash, the same hash space as a drawable's file
    /// stem.
    pub name_hash: u32,
    /// `textureDictionary` — 0 when the archetype names none.
    pub texture_dict_hash: u32,
}

pub(crate) struct MetaBlock {
    pub(crate) name_hash: u32,
    pub(crate) data: Vec<u8>,
    /// Where the block starts in the system section, when it lives there.
    pub(crate) system_offset: Option<usize>,
}

/// Decodes a packed Meta-format pointer (distinct from the resource VAs
/// used elsewhere: `block_id` in the low 12 bits, 1-based, an offset in the
/// next 20) into a zero-based block index and byte offset. `None` for a
/// null or zero-block pointer.
pub(crate) fn decode_meta_pointer(raw: u64) -> Option<(usize, usize)> {
    let block_id = (raw & 0xFFF) as usize;
    if block_id == 0 {
        return None;
    }
    let offset = ((raw >> 12) & 0xFFFFF) as usize;
    Some((block_id - 1, offset))
}

/// Reads every `MetaDataBlock` out of a .ytyp's `Meta` header at
/// `SYSTEM_BASE`. The header is 0x70 (112) bytes; the fields we need are
/// `DataBlocksPointer` at 0x30 and `DataBlocksCount` at 0x4C.
pub(crate) fn read_meta_blocks(reader: &ResReader<'_>) -> Result<Vec<MetaBlock>> {
    let header = reader.resolve(SYSTEM_BASE, 0x70).context("ytyp: Meta header out of bounds")?;

    let data_blocks_pointer = u64_le(header, 0x30);
    let data_blocks_count = u16_le(header, 0x4C) as usize;

    if data_blocks_pointer == 0 || data_blocks_count == 0 {
        bail!("ytyp: no data blocks");
    }

    let block_headers = reader
        .resolve(data_blocks_pointer, data_blocks_count.checked_mul(16).context("ytyp: data block count overflow")?)
        .context("ytyp: data block array out of bounds")?;

    let mut blocks = Vec::with_capacity(data_blocks_count);
    for i in 0..data_blocks_count {
        let off = i * 16;
        let name_hash = u32_le(block_headers, off);
        let length = u32_le(block_headers, off + 4) as usize;
        let data_ptr = u64_le(block_headers, off + 8);
        // A block whose data pointer doesn't resolve is skipped rather than
        // failing the whole file — other blocks may still be usable.
        let data = reader.resolve(data_ptr, length).map(|b| b.to_vec()).unwrap_or_default();
        let system_offset = (data_ptr >= SYSTEM_BASE && data_ptr < crate::resource::GRAPHICS_BASE).then(|| (data_ptr - SYSTEM_BASE) as usize);
        blocks.push(MetaBlock { name_hash, data, system_offset });
    }

    Ok(blocks)
}

/// Reads a Meta array field — a packed pointer at `off` in `rec`, its entry
/// count eight bytes later — as `stride`-byte records, stopping early rather
/// than failing if the target block runs out.
pub(crate) fn meta_array_records<'a>(
    blocks: &'a [MetaBlock],
    rec: &[u8],
    off: usize,
    stride: usize,
) -> Vec<&'a [u8]> {
    let count = u16_le(rec, off + 8) as usize;
    let Some((bi, boff)) = decode_meta_pointer(u64_le(rec, off)) else { return Vec::new() };
    let Some(block) = blocks.get(bi) else { return Vec::new() };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let Some(r) = block.data.get(boff + i * stride..boff + (i + 1) * stride) else { break };
        out.push(r);
    }
    out
}

/// Reads an `Array_uint` field (packed pointer at `off`, count at `off + 8`).
pub(crate) fn read_meta_u32_array(blocks: &[MetaBlock], rec: &[u8], off: usize) -> Vec<u32> {
    meta_array_records(blocks, rec, off, 4).iter().map(|v| u32_le(v, 0)).collect()
}

/// Follows a `CharPointer` field (packed pointer at `off`, byte count at
/// `off + 8`) into the file's string block. The target block's structure-name
/// hash is not checked: it is `STRING` in every file seen, but nothing in the
/// format requires that. `None` when the pointer doesn't resolve.
pub(crate) fn read_meta_string(blocks: &[MetaBlock], rec: &[u8], off: usize) -> Option<String> {
    let len = u16_le(rec, off + 8) as usize;
    let (bi, boff) = decode_meta_pointer(u64_le(rec, off))?;
    let bytes = blocks.get(bi)?.data.get(boff..boff + len)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(len);
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Reads an `Array_StructurePointer` of `CEntityDef`s (the MLO's own entity
/// list, and each entity set's).
pub(crate) fn read_entity_pointers(blocks: &[MetaBlock], rec: &[u8], off: usize) -> Vec<YmapEntity> {
    meta_array_records(blocks, rec, off, 8)
        .iter()
        .filter_map(|ptr| {
            let (bi, boff) = decode_meta_pointer(u64_le(ptr, 0))?;
            let block = blocks.get(bi)?;
            if block.name_hash != HASH_CENTITYDEF {
                return None;
            }
            Some(read_entity(block.data.get(boff..boff + 128)?, false))
        })
        .collect()
}

/// Extracts every archetype's `(name hash, texture dictionary hash)` from a
/// .ytyp's raw bytes.
pub fn parse_archetype_txds(data: &[u8]) -> Result<Vec<ArchetypeTxd>> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    parse_archetype_txds_from_reader(&reader)
}

fn parse_archetype_txds_from_reader(reader: &ResReader<'_>) -> Result<Vec<ArchetypeTxd>> {
    let blocks = read_meta_blocks(reader)?;

    let cmaptypes = blocks
        .iter()
        .find(|b| b.name_hash == HASH_CMAPTYPES)
        .context("ytyp: CMapTypes block not found")?;

    // CMapTypes is 80 bytes; `archetypes` (an Array_StructurePointer: a
    // packed pointer + two u16 counts) sits at offset 24.
    const ARCHETYPES_FIELD_OFFSET: usize = 24;
    if cmaptypes.data.len() < ARCHETYPES_FIELD_OFFSET + 16 {
        bail!("ytyp: CMapTypes block too small");
    }

    let archetypes_pointer = u64_le(&cmaptypes.data, ARCHETYPES_FIELD_OFFSET);
    let archetypes_count = u16_le(&cmaptypes.data, ARCHETYPES_FIELD_OFFSET + 8) as usize;

    let Some((arr_block_idx, arr_offset)) = decode_meta_pointer(archetypes_pointer) else {
        return Ok(Vec::new());
    };
    let Some(arr_block) = blocks.get(arr_block_idx) else {
        bail!("ytyp: archetypes pointer block out of range");
    };

    let ptr_bytes_len = archetypes_count.checked_mul(8).context("ytyp: archetype count overflow")?;
    let Some(ptr_bytes) = arr_block.data.get(arr_offset..arr_offset + ptr_bytes_len) else {
        bail!("ytyp: archetypes pointer array out of bounds");
    };

    // The three archetype-def structure kinds all begin with the same
    // 144-byte `CBaseArchetypeDef` layout; only what follows differs, and
    // we don't read past byte 144.
    const BASE_ARCHETYPE_DEF_LEN: usize = 144;
    const NAME_OFFSET: usize = 88;
    const TEXTURE_DICT_OFFSET: usize = 92;
    const ASSET_NAME_OFFSET: usize = 112;

    let mut out = Vec::with_capacity(archetypes_count);
    for i in 0..archetypes_count {
        let ptr = u64_le(ptr_bytes, i * 8);
        let Some((a_block_idx, a_offset)) = decode_meta_pointer(ptr) else { continue };
        let Some(block) = blocks.get(a_block_idx) else { continue };
        if !matches!(
            block.name_hash,
            HASH_CBASE_ARCHETYPE_DEF | HASH_CTIME_ARCHETYPE_DEF | HASH_CMLO_ARCHETYPE_DEF
        ) {
            continue;
        }
        let Some(base) = block.data.get(a_offset..a_offset + BASE_ARCHETYPE_DEF_LEN) else { continue };

        let mut name_hash = u32_le(base, NAME_OFFSET);
        let texture_dict_hash = u32_le(base, TEXTURE_DICT_OFFSET);
        if name_hash == 0 {
            // The reference tool Archetype.cs: `Hash = arch.assetName` when `name` is 0.
            name_hash = u32_le(base, ASSET_NAME_OFFSET);
        }

        out.push(ArchetypeTxd { name_hash, texture_dict_hash });
    }

    Ok(out)
}

/// An archetype's placement-relevant fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Archetype {
    /// `name` (or `assetName` when `name` is 0), lowercase JOAAT.
    pub name_hash: u32,
    pub bb_min: Vec3,
    pub bb_max: Vec3,
    pub lod_dist: f32,
    /// `textureDictionary` hash, 0 when none.
    pub texture_dict_hash: u32,
    /// `drawableDictionary` hash: the `.ydd` holding the model when
    /// `asset_type` says so; 0 otherwise.
    pub drawable_dictionary_hash: u32,
    /// `assetName` hash: the model's own name (its `.ydr`/`.yft` stem, or
    /// its member name inside the drawable dictionary).
    pub asset_name_hash: u32,
    /// `assetType` as stored: see the `ASSET_TYPE_*` constants.
    pub asset_type: u32,
    pub is_mlo: bool,
}

impl Archetype {
    pub const ASSET_TYPE_UNINITIALIZED: u32 = 0;
    pub const ASSET_TYPE_FRAGMENT: u32 = 1;
    pub const ASSET_TYPE_DRAWABLE: u32 = 2;
    pub const ASSET_TYPE_DRAWABLEDICTIONARY: u32 = 3;
    pub const ASSET_TYPE_ASSETLESS: u32 = 4;

    /// The model lives inside a `.ydd` named by `drawable_dictionary_hash`.
    pub fn in_drawable_dictionary(&self) -> bool {
        self.asset_type == Self::ASSET_TYPE_DRAWABLEDICTIONARY && self.drawable_dictionary_hash != 0
    }

    /// The model is a fragment (`.yft`).
    pub fn is_fragment(&self) -> bool {
        self.asset_type == Self::ASSET_TYPE_FRAGMENT
    }

    /// The hash of the file (or dictionary member) that holds the model:
    /// `assetName` when set, else the archetype's own name.
    pub fn model_hash(&self) -> u32 {
        if self.asset_name_hash != 0 { self.asset_name_hash } else { self.name_hash }
    }
}

/// A room of an MLO, in MLO-local space.
#[derive(Debug, Clone, PartialEq)]
pub struct MloRoom {
    /// The room's own name, e.g. "limbo" or "Kitchen" — empty when the file's
    /// string block doesn't resolve.
    pub name: String,
    pub bb_min: Vec3,
    pub bb_max: Vec3,
    pub flags: u32,
    pub floor_id: i32,
    /// Indices into the MLO's `entities` list of the entities this room owns.
    pub attached_objects: Vec<u32>,
}

/// A doorway (or window, or any other see-through join) between two of an
/// MLO's rooms, as a quad of corners in MLO-local space.
#[derive(Debug, Clone, PartialEq)]
pub struct MloPortal {
    /// Index into the MLO's `rooms`; room 0 is "limbo", the world outside.
    pub room_from: u32,
    pub room_to: u32,
    pub flags: u32,
    pub mirror_priority: u32,
    /// 0..=255; how much the portal occludes what is behind it. Stored as an
    /// integer in the Meta layout (the reference tool `MetaTypes.cs`: `UnsignedInt` at
    /// 24), unlike the PSO form of the same field.
    pub opacity: u32,
    pub audio_occlusion: u32,
    /// The portal's corners, normally four, in winding order.
    pub corners: Vec<Vec3>,
    /// Indices into the MLO's `entities` list (doors, glass) attached here.
    pub attached_objects: Vec<u32>,
}

impl MloPortal {
    /// True when the portal opens onto room 0 ("limbo"), i.e. it is a way in
    /// or out of the interior rather than a join between two of its rooms.
    pub fn is_exterior(&self) -> bool {
        self.room_from == 0 || self.room_to == 0
    }
}

/// A named, separately streamed group of an MLO's entities (a "DLC" furniture
/// set, a destroyed variant…) that the game switches on and off as a unit.
#[derive(Debug, Clone, PartialEq)]
pub struct MloEntitySet {
    /// Lowercase JOAAT of the set's name.
    pub name_hash: u32,
    /// One room index per entity, telling which room each of `entities` is in.
    pub locations: Vec<u32>,
    pub entities: Vec<YmapEntity>,
}

/// A `CMloArchetypeDef`: the interior's own entities (props, doors,
/// furniture) placed relative to the MLO origin, its rooms, the portals
/// between them and its switchable entity sets.
#[derive(Debug, Clone, PartialEq)]
pub struct MloDef {
    pub name_hash: u32,
    pub entities: Vec<YmapEntity>,
    pub rooms: Vec<MloRoom>,
    pub portals: Vec<MloPortal>,
    pub entity_sets: Vec<MloEntitySet>,
}

/// Everything a `.ytyp` declares that placement work needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ytyp {
    pub archetypes: Vec<Archetype>,
    pub mlos: Vec<MloDef>,
}

/// Parses every archetype (with bounding box) and every MLO definition
/// (with its entities and rooms) out of a `.ytyp`.
pub fn parse_ytyp(data: &[u8]) -> Result<Ytyp> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    let blocks = read_meta_blocks(&reader)?;
    let cmaptypes = blocks.iter().find(|b| b.name_hash == HASH_CMAPTYPES).context("ytyp: CMapTypes block not found")?;
    const ARCHETYPES_FIELD_OFFSET: usize = 24;
    if cmaptypes.data.len() < ARCHETYPES_FIELD_OFFSET + 16 {
        bail!("ytyp: CMapTypes block too small");
    }
    let archetypes_pointer = u64_le(&cmaptypes.data, ARCHETYPES_FIELD_OFFSET);
    let archetypes_count = u16_le(&cmaptypes.data, ARCHETYPES_FIELD_OFFSET + 8) as usize;
    let Some((arr_block_idx, arr_offset)) = decode_meta_pointer(archetypes_pointer) else { return Ok(Ytyp::default()) };
    let arr_block = blocks.get(arr_block_idx).context("ytyp: archetypes pointer block out of range")?;
    let ptr_bytes = arr_block.data.get(arr_offset..arr_offset + archetypes_count * 8).context("ytyp: archetypes pointer array out of bounds")?;

    let mut out = Ytyp::default();
    for i in 0..archetypes_count {
        let Some((bi, off)) = decode_meta_pointer(u64_le(ptr_bytes, i * 8)) else { continue };
        let Some(block) = blocks.get(bi) else { continue };
        let is_mlo = match block.name_hash {
            HASH_CBASE_ARCHETYPE_DEF | HASH_CTIME_ARCHETYPE_DEF => false,
            HASH_CMLO_ARCHETYPE_DEF => true,
            _ => continue,
        };
        let Some(base) = block.data.get(off..off + 144) else { continue };
        let mut name_hash = u32_le(base, 88);
        if name_hash == 0 { name_hash = u32_le(base, 112); }
        out.archetypes.push(Archetype {
            name_hash,
            bb_min: vec3_le(base, 32),
            bb_max: vec3_le(base, 48),
            lod_dist: f32_le(base, 8),
            texture_dict_hash: u32_le(base, 92),
            drawable_dictionary_hash: u32_le(base, 100),
            asset_type: u32_le(base, 108),
            asset_name_hash: u32_le(base, 112),
            is_mlo,
        });
        if !is_mlo { continue; }
        let Some(mlo) = block.data.get(off..off + 240) else { continue };

        // entities: Array_StructurePointer of CEntityDef at 152.
        let entities = read_entity_pointers(&blocks, mlo, 152);

        // rooms: Array_Structure of 112-byte CMloRoomDef at 168.
        let rooms = meta_array_records(&blocks, mlo, 168, 112)
            .iter()
            .map(|r| MloRoom {
                name: read_meta_string(&blocks, r, 8).unwrap_or_default(),
                bb_min: vec3_le(r, 32),
                bb_max: vec3_le(r, 48),
                flags: u32_le(r, 76),
                floor_id: u32_le(r, 84) as i32,
                attached_objects: read_meta_u32_array(&blocks, r, 96),
            })
            .collect();

        // portals: Array_Structure of 64-byte CMloPortalDef at 184.
        let portals = meta_array_records(&blocks, mlo, 184, 64)
            .iter()
            .map(|p| MloPortal {
                room_from: u32_le(p, 8),
                room_to: u32_le(p, 12),
                flags: u32_le(p, 16),
                mirror_priority: u32_le(p, 20),
                opacity: u32_le(p, 24),
                audio_occlusion: u32_le(p, 28),
                // Corners are stored as Vector4s; the w is padding.
                corners: meta_array_records(&blocks, p, 32, 16).iter().map(|c| vec3_le(c, 0)).collect(),
                attached_objects: read_meta_u32_array(&blocks, p, 48),
            })
            .collect();

        // entitySets: Array_Structure of 48-byte CMloEntitySet at 200.
        let entity_sets = meta_array_records(&blocks, mlo, 200, 48)
            .iter()
            .map(|s| MloEntitySet {
                name_hash: u32_le(s, 8),
                locations: read_meta_u32_array(&blocks, s, 16),
                entities: read_entity_pointers(&blocks, s, 32),
            })
            .collect();

        out.mlos.push(MloDef { name_hash, entities, rooms, portals, entity_sets });
    }
    Ok(out)
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod tests {
    use super::*;
    use crate::hash::rage_joaat;
    use crate::resource::build_rsc7;

    #[test]
    fn structure_hashes_match_references_metaname_enum() {
        // MetaName hashes are the RAGE Jenkins hash of the exact-case
        // structure name (not lowercased) -- verified against the reference tool's
        // MetaNames.cs constants.
        assert_eq!(rage_joaat_exact_case("CMapTypes"), HASH_CMAPTYPES);
        assert_eq!(rage_joaat_exact_case("CBaseArchetypeDef"), HASH_CBASE_ARCHETYPE_DEF);
        assert_eq!(rage_joaat_exact_case("CTimeArchetypeDef"), HASH_CTIME_ARCHETYPE_DEF);
        assert_eq!(rage_joaat_exact_case("CMloArchetypeDef"), HASH_CMLO_ARCHETYPE_DEF);
        assert_eq!(rage_joaat_exact_case("CMloRoomDef"), HASH_CMLO_ROOM_DEF);
        assert_eq!(rage_joaat_exact_case("CMloPortalDef"), HASH_CMLO_PORTAL_DEF);
        assert_eq!(rage_joaat_exact_case("CMloEntitySet"), HASH_CMLO_ENTITY_SET);
        assert_eq!(rage_joaat_exact_case("STRING"), HASH_STRING);
    }

    // `rage_joaat` documents its input as "should already be lowercase", but
    // the hash function itself is case-agnostic about that contract -- it
    // just hashes whatever bytes it's given. MetaName hashes are exact-case.
    fn rage_joaat_exact_case(s: &str) -> u32 {
        rage_joaat(s)
    }

    #[test]
    fn decode_meta_pointer_rejects_zero_block_id() {
        assert_eq!(decode_meta_pointer(0), None);
        // block_id occupies the low 12 bits; 0 there means null regardless
        // of a nonzero offset in the upper bits.
        assert_eq!(decode_meta_pointer(0x1000), None);
    }

    #[test]
    fn decode_meta_pointer_splits_block_id_and_offset() {
        // block_id = 1 (-> index 0), offset = 0x10
        let raw = 1u64 | (0x10u64 << 12);
        assert_eq!(decode_meta_pointer(raw), Some((0, 0x10)));

        // block_id = 3 (-> index 2), offset = 0
        assert_eq!(decode_meta_pointer(3), Some((2, 0)));
    }

    /// Builds a minimal .ytyp-shaped `Meta` system section: a `CMapTypes`
    /// block, a block holding the archetypes-pointer array (its own block,
    /// as the real format has it — the array is not inline in `CMapTypes`),
    /// and one `CBaseArchetypeDef` block.
    fn build_ytyp_system(name_hash: u32, texture_dict_hash: u32, asset_name_hash: u32) -> Vec<u8> {
        // Layout (all offsets are byte offsets within `system`; resource
        // VAs are SYSTEM_BASE + offset):
        //   0x000..0x070  Meta header
        //   0x070..0x0A0  DataBlocks array: 3 x MetaDataBlock (16 bytes each)
        //   0x0A0..0x0F0  block 0: CMapTypes data (80 bytes)
        //   0x0F0..0x0F8  block 1: archetypes pointer array (1 x u64)
        //   0x0F8..0x188  block 2: CBaseArchetypeDef data (144 bytes)
        let mut sys = vec![0u8; 0x188];

        // Meta header.
        sys[0x30..0x38].copy_from_slice(&(SYSTEM_BASE + 0x070).to_le_bytes()); // DataBlocksPointer
        sys[0x4C..0x4E].copy_from_slice(&3u16.to_le_bytes()); // DataBlocksCount

        // DataBlocks[0]: CMapTypes.
        sys[0x070..0x074].copy_from_slice(&HASH_CMAPTYPES.to_le_bytes());
        sys[0x074..0x078].copy_from_slice(&80u32.to_le_bytes());
        sys[0x078..0x080].copy_from_slice(&(SYSTEM_BASE + 0x0A0).to_le_bytes());

        // DataBlocks[1]: archetypes pointer array. Its structure-name hash
        // doesn't matter to the parser (only archetype-def blocks are
        // matched by hash), so it's left 0.
        sys[0x080..0x084].copy_from_slice(&0u32.to_le_bytes());
        sys[0x084..0x088].copy_from_slice(&8u32.to_le_bytes());
        sys[0x088..0x090].copy_from_slice(&(SYSTEM_BASE + 0x0F0).to_le_bytes());

        // DataBlocks[2]: CBaseArchetypeDef.
        sys[0x090..0x094].copy_from_slice(&HASH_CBASE_ARCHETYPE_DEF.to_le_bytes());
        sys[0x094..0x098].copy_from_slice(&144u32.to_le_bytes());
        sys[0x098..0x0A0].copy_from_slice(&(SYSTEM_BASE + 0x0F8).to_le_bytes());

        // CMapTypes.archetypes (offset 24 within the 80-byte struct):
        // packed pointer -> block index 1 (encoded as block_id=2), offset 0
        // within that block's data; count1 = 1.
        let archetypes_field = 0x0A0 + 24;
        let packed_ptr = 2u64; // block_id=2 -> index 1, offset=0
        sys[archetypes_field..archetypes_field + 8].copy_from_slice(&packed_ptr.to_le_bytes());
        sys[archetypes_field + 8..archetypes_field + 10].copy_from_slice(&1u16.to_le_bytes());

        // archetypes pointer array (block index 1): one packed pointer ->
        // block index 2 (block_id=3), offset 0.
        let packed_arch_ptr = 3u64; // block_id=3 -> index 2, offset=0
        sys[0x0F0..0x0F8].copy_from_slice(&packed_arch_ptr.to_le_bytes());

        // CBaseArchetypeDef fields (block index 2, base 0x0F8).
        sys[0x0F8 + 88..0x0F8 + 92].copy_from_slice(&name_hash.to_le_bytes());
        sys[0x0F8 + 92..0x0F8 + 96].copy_from_slice(&texture_dict_hash.to_le_bytes());
        sys[0x0F8 + 112..0x0F8 + 116].copy_from_slice(&asset_name_hash.to_le_bytes());

        sys
    }

    #[test]
    fn parses_one_archetypes_texture_dictionary() {
        let system = build_ytyp_system(rage_joaat("prop_table_02"), rage_joaat("prop_tableset_02"), 0);
        let reader = ResReader { system: &system, graphics: &[] };
        let archetypes = parse_archetype_txds_from_reader(&reader).expect("should parse");
        assert_eq!(archetypes.len(), 1);
        assert_eq!(archetypes[0].name_hash, rage_joaat("prop_table_02"));
        assert_eq!(archetypes[0].texture_dict_hash, rage_joaat("prop_tableset_02"));
    }

    #[test]
    fn falls_back_to_asset_name_when_name_is_zero() {
        let system = build_ytyp_system(0, rage_joaat("prop_tableset_02"), rage_joaat("prop_table_02"));
        let reader = ResReader { system: &system, graphics: &[] };
        let archetypes = parse_archetype_txds_from_reader(&reader).expect("should parse");
        assert_eq!(archetypes.len(), 1);
        assert_eq!(archetypes[0].name_hash, rage_joaat("prop_table_02"));
    }

    // ─── MLO fixture ──────────────────────────────────────────────────────────

    fn put_u16(s: &mut [u8], off: usize, v: u16) { s[off..off + 2].copy_from_slice(&v.to_le_bytes()); }
    fn put_u32(s: &mut [u8], off: usize, v: u32) { s[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
    fn put_u64(s: &mut [u8], off: usize, v: u64) { s[off..off + 8].copy_from_slice(&v.to_le_bytes()); }
    fn put_f32(s: &mut [u8], off: usize, v: f32) { s[off..off + 4].copy_from_slice(&v.to_le_bytes()); }
    fn put_vec3(s: &mut [u8], off: usize, v: Vec3) {
        put_f32(s, off, v.x);
        put_f32(s, off + 4, v.y);
        put_f32(s, off + 8, v.z);
    }

    /// A packed Meta pointer to `block_offset` bytes into block `block_index`
    /// (the inverse of [`decode_meta_pointer`]).
    fn meta_ptr(block_index: usize, block_offset: usize) -> u64 {
        (block_index as u64 + 1) | ((block_offset as u64) << 12)
    }

    /// A Meta array (or `CharPointer`) field: packed pointer at `off`, `count1`
    /// eight bytes later.
    fn put_array(s: &mut [u8], off: usize, block_index: usize, block_offset: usize, count: u16) {
        put_u64(s, off, meta_ptr(block_index, block_offset));
        put_u16(s, off + 8, count);
    }

    /// `(structure-name hash, offset, length)` of each data block in
    /// [`build_mlo_system`]'s section, in the order Meta pointers name them.
    const MLO_BLOCKS: [(u32, usize, usize); 12] = [
        (HASH_CMAPTYPES, 0x130, 80),           //  0: CMapTypes
        (0, 0x180, 8),                         //  1: archetypes pointer array
        (HASH_CMLO_ARCHETYPE_DEF, 0x190, 240), //  2: the MLO archetype
        (HASH_STRING, 0x280, 13),              //  3: "kitchen\0hall\0"
        (HASH_CMLO_ROOM_DEF, 0x290, 224),      //  4: 2 x CMloRoomDef
        (HASH_CMLO_PORTAL_DEF, 0x370, 64),     //  5: 1 x CMloPortalDef
        (0, 0x3B0, 64),                        //  6: 4 x Vector4 (portal corners)
        (0, 0x3F0, 8),                         //  7: u32 array [0, 1]
        (HASH_CMLO_ENTITY_SET, 0x400, 48),     //  8: 1 x CMloEntitySet
        (0, 0x430, 8),                         //  9: the entity set's entity pointers
        (HASH_CENTITYDEF, 0x440, 128),         // 10: the one CEntityDef
        (0, 0x4C0, 8),                         // 11: the MLO's own entity pointers
    ];

    /// The corners of [`minimal_mlo_ytyp`]'s one portal.
    const MLO_PORTAL_CORNERS: [Vec3; 4] = [
        Vec3 { x: 4.0, y: 0.0, z: 0.0 },
        Vec3 { x: 4.0, y: 3.0, z: 0.0 },
        Vec3 { x: 4.0, y: 3.0, z: 2.5 },
        Vec3 { x: 4.0, y: 0.0, z: 2.5 },
    ];

    /// The archetype name of the [`minimal_mlo_ytyp`] interior.
    pub const MINIMAL_MLO_NAME: &str = "v_test_mlo";

    /// Builds the Meta system section of [`minimal_mlo_ytyp`].
    fn build_mlo_system() -> Vec<u8> {
        let mut sys = vec![0u8; 0x500];

        // Meta header: DataBlocksPointer at 0x30, DataBlocksCount at 0x4C.
        put_u64(&mut sys, 0x30, SYSTEM_BASE + 0x70);
        put_u16(&mut sys, 0x4C, MLO_BLOCKS.len() as u16);
        for (i, &(hash, off, len)) in MLO_BLOCKS.iter().enumerate() {
            let h = 0x70 + i * 16;
            put_u32(&mut sys, h, hash);
            put_u32(&mut sys, h + 4, len as u32);
            put_u64(&mut sys, h + 8, SYSTEM_BASE + off as u64);
        }

        // CMapTypes.archetypes -> block 1, one entry, which points at the
        // CMloArchetypeDef in block 2.
        put_array(&mut sys, MLO_BLOCKS[0].1 + 24, 1, 0, 1);
        put_u64(&mut sys, MLO_BLOCKS[1].1, meta_ptr(2, 0));

        // CMloArchetypeDef.
        let a = MLO_BLOCKS[2].1;
        put_f32(&mut sys, a + 8, 100.0); // lodDist
        put_vec3(&mut sys, a + 32, Vec3::new(0.0, 0.0, 0.0)); // bbMin
        put_vec3(&mut sys, a + 48, Vec3::new(9.0, 3.0, 2.5)); // bbMax
        put_u32(&mut sys, a + 88, rage_joaat(MINIMAL_MLO_NAME)); // name
        put_array(&mut sys, a + 152, 11, 0, 1); // entities
        put_array(&mut sys, a + 168, 4, 0, 2); // rooms
        put_array(&mut sys, a + 184, 5, 0, 1); // portals
        put_array(&mut sys, a + 200, 8, 0, 1); // entitySets

        // The STRING block the rooms' name pointers aim into.
        let s = MLO_BLOCKS[3].1;
        sys[s..s + 13].copy_from_slice(b"kitchen\0hall\0");

        // CMloRoomDef[0] "kitchen", and [1] "hall" with one attached object.
        let r = MLO_BLOCKS[4].1;
        put_array(&mut sys, r + 8, 3, 0, 8); // name -> "kitchen\0"
        put_vec3(&mut sys, r + 32, Vec3::new(0.0, 0.0, 0.0));
        put_vec3(&mut sys, r + 48, Vec3::new(4.0, 3.0, 2.5));
        put_f32(&mut sys, r + 64, 1.0); // blend
        put_u32(&mut sys, r + 76, 1); // flags
        put_u32(&mut sys, r + 80, 1); // portalCount
        put_u32(&mut sys, r + 84, 0); // floorId

        let r1 = r + 112;
        put_array(&mut sys, r1 + 8, 3, 8, 5); // name -> "hall\0"
        put_vec3(&mut sys, r1 + 32, Vec3::new(4.0, 0.0, 0.0));
        put_vec3(&mut sys, r1 + 48, Vec3::new(9.0, 3.0, 2.5));
        put_u32(&mut sys, r1 + 76, 2); // flags
        put_u32(&mut sys, r1 + 80, 1); // portalCount
        put_u32(&mut sys, r1 + 84, 1); // floorId
        put_array(&mut sys, r1 + 96, 7, 0, 1); // attachedObjects -> [0]

        // The one CMloPortalDef, joining room 1 to room 2.
        let p = MLO_BLOCKS[5].1;
        put_u32(&mut sys, p + 8, 1); // roomFrom
        put_u32(&mut sys, p + 12, 2); // roomTo
        put_u32(&mut sys, p + 16, 4); // flags
        put_u32(&mut sys, p + 20, 3); // mirrorPriority
        put_u32(&mut sys, p + 24, 128); // opacity
        put_u32(&mut sys, p + 28, 7); // audioOcclusion
        put_array(&mut sys, p + 32, 6, 0, 4); // corners

        // Its four corners, stored as Vector4 (the w is padding).
        let c = MLO_BLOCKS[6].1;
        for (i, corner) in MLO_PORTAL_CORNERS.iter().enumerate() {
            put_vec3(&mut sys, c + i * 16, *corner);
        }

        // The shared u32 array: [0] is room 1's attached object, [1] is the
        // entity set's one location.
        let u = MLO_BLOCKS[7].1;
        put_u32(&mut sys, u, 0);
        put_u32(&mut sys, u + 4, 1);

        // The one CMloEntitySet, and the pointer array of its entities.
        let e = MLO_BLOCKS[8].1;
        put_u32(&mut sys, e + 8, 0xABCD); // name
        put_array(&mut sys, e + 16, 7, 4, 1); // locations -> [1]
        put_array(&mut sys, e + 32, 9, 0, 1); // entities
        put_u64(&mut sys, MLO_BLOCKS[9].1, meta_ptr(10, 0));

        // The CEntityDef both the MLO and its entity set point at.
        let n = MLO_BLOCKS[10].1;
        put_u32(&mut sys, n + 8, rage_joaat("prop_table_02")); // archetypeName
        put_vec3(&mut sys, n + 32, Vec3::new(1.0, 2.0, 3.0)); // position
        put_f32(&mut sys, n + 60, 1.0); // rotation w
        put_f32(&mut sys, n + 64, 1.0); // scaleXY
        put_f32(&mut sys, n + 68, 1.0); // scaleZ
        put_u32(&mut sys, n + 72, -1i32 as u32); // parentIndex
        put_f32(&mut sys, n + 76, 60.0); // lodDist
        put_u64(&mut sys, MLO_BLOCKS[11].1, meta_ptr(10, 0));

        sys
    }

    /// A hand-built `.ytyp` holding one `CMloArchetypeDef`: two named rooms
    /// ("kitchen" and "hall"), the portal between them, one entity set and one
    /// placed entity — the smallest file exercising every MLO field this
    /// module reads.
    pub fn minimal_mlo_ytyp() -> Vec<u8> {
        build_rsc7(2, &build_mlo_system(), &[])
    }

    #[test]
    fn parses_mlo_rooms_with_names() {
        let ytyp = parse_ytyp(&minimal_mlo_ytyp()).expect("fixture should parse");
        assert_eq!(ytyp.archetypes.len(), 1);
        assert!(ytyp.archetypes[0].is_mlo);
        assert_eq!(ytyp.mlos.len(), 1);
        let mlo = &ytyp.mlos[0];
        assert_eq!(mlo.name_hash, rage_joaat(MINIMAL_MLO_NAME));

        let names: Vec<&str> = mlo.rooms.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["kitchen", "hall"]);
        assert_eq!(mlo.rooms[0].bb_min, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(mlo.rooms[0].bb_max, Vec3::new(4.0, 3.0, 2.5));
        assert_eq!(mlo.rooms[0].flags, 1);
        assert_eq!(mlo.rooms[0].floor_id, 0);
        assert!(mlo.rooms[0].attached_objects.is_empty());
        assert_eq!(mlo.rooms[1].bb_min, Vec3::new(4.0, 0.0, 0.0));
        assert_eq!(mlo.rooms[1].bb_max, Vec3::new(9.0, 3.0, 2.5));
        assert_eq!(mlo.rooms[1].flags, 2);
        assert_eq!(mlo.rooms[1].floor_id, 1);
        assert_eq!(mlo.rooms[1].attached_objects, vec![0]);
    }

    #[test]
    fn parses_mlo_portals_with_corners() {
        let ytyp = parse_ytyp(&minimal_mlo_ytyp()).expect("fixture should parse");
        let portals = &ytyp.mlos[0].portals;
        assert_eq!(portals.len(), 1);
        let p = &portals[0];
        assert_eq!(p.room_from, 1);
        assert_eq!(p.room_to, 2);
        assert_eq!(p.flags, 4);
        assert_eq!(p.mirror_priority, 3);
        assert_eq!(p.opacity, 128);
        assert_eq!(p.audio_occlusion, 7);
        assert_eq!(p.corners, MLO_PORTAL_CORNERS.to_vec());
        assert!(p.attached_objects.is_empty());
        // Neither side is room 0 (limbo, i.e. the world outside the MLO).
        assert!(!p.is_exterior());
    }

    #[test]
    fn portal_touching_room_zero_is_exterior() {
        let inside = MloPortal {
            room_from: 1, room_to: 2, flags: 0, mirror_priority: 0, opacity: 255,
            audio_occlusion: 0, corners: Vec::new(), attached_objects: Vec::new(),
        };
        assert!(!inside.is_exterior());
        assert!(MloPortal { room_from: 0, ..inside.clone() }.is_exterior());
        assert!(MloPortal { room_to: 0, ..inside }.is_exterior());
    }

    #[test]
    fn parses_mlo_entity_sets_and_entities() {
        let ytyp = parse_ytyp(&minimal_mlo_ytyp()).expect("fixture should parse");
        let mlo = &ytyp.mlos[0];

        assert_eq!(mlo.entities.len(), 1);
        assert_eq!(mlo.entities[0].archetype_hash, rage_joaat("prop_table_02"));
        assert_eq!(mlo.entities[0].position, Vec3::new(1.0, 2.0, 3.0));

        assert_eq!(mlo.entity_sets.len(), 1);
        let set = &mlo.entity_sets[0];
        assert_eq!(set.name_hash, 0xABCD);
        assert_eq!(set.locations, vec![1]);
        assert_eq!(set.entities.len(), 1);
        assert_eq!(set.entities[0].position, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn minimal_mlo_ytyp_round_trips_through_rsc7() {
        let file = minimal_mlo_ytyp();
        let (system, graphics) = prepare_rsc7(&file).expect("fixture should be a valid RSC7");
        let blocks = read_meta_blocks(&ResReader { system: &system, graphics: &graphics })
            .expect("fixture should carry data blocks");
        assert_eq!(blocks.len(), MLO_BLOCKS.len());
        assert_eq!(blocks[4].name_hash, HASH_CMLO_ROOM_DEF);
        assert_eq!(blocks[4].data.len(), 224);
    }

    #[test]
    fn archetype_only_ytyp_still_parses_with_no_mlos() {
        let file = build_rsc7(2, &build_ytyp_system(rage_joaat("prop_table_02"), 0, 0), &[]);
        let ytyp = parse_ytyp(&file).expect("should parse");
        assert_eq!(ytyp.archetypes.len(), 1);
        assert!(!ytyp.archetypes[0].is_mlo);
        assert!(ytyp.mlos.is_empty());
    }

    #[test]
    fn mlo_with_null_arrays_yields_empty_vecs() {
        let mut sys = build_mlo_system();
        let a = MLO_BLOCKS[2].1;
        for field in [152usize, 168, 184, 200] {
            put_u64(&mut sys, a + field, 0);
            put_u16(&mut sys, a + field + 8, 0);
        }
        let ytyp = parse_ytyp(&build_rsc7(2, &sys, &[])).expect("should parse");
        let mlo = &ytyp.mlos[0];
        assert!(mlo.entities.is_empty());
        assert!(mlo.rooms.is_empty());
        assert!(mlo.portals.is_empty());
        assert!(mlo.entity_sets.is_empty());
    }

    #[test]
    fn truncated_room_block_stops_short_instead_of_panicking() {
        let mut sys = build_mlo_system();
        // Shorten the rooms block so only the first of its two rooms fits.
        put_u32(&mut sys, 0x70 + 4 * 16 + 4, 112 + 8);
        let ytyp = parse_ytyp(&build_rsc7(2, &sys, &[])).expect("should parse");
        let rooms = &ytyp.mlos[0].rooms;
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].name, "kitchen");
    }

    #[test]
    fn room_name_pointing_at_a_missing_block_reads_as_empty() {
        let mut sys = build_mlo_system();
        // Aim room 0's name at a block index past the end of the table.
        put_u64(&mut sys, MLO_BLOCKS[4].1 + 8, meta_ptr(64, 0));
        let ytyp = parse_ytyp(&build_rsc7(2, &sys, &[])).expect("should parse");
        assert_eq!(ytyp.mlos[0].rooms[0].name, "");
        assert_eq!(ytyp.mlos[0].rooms[1].name, "hall");
    }
}
