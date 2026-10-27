use anyhow::{Result, Context, bail};
use crate::resource::{prepare_rsc7, u16_le, u32_le, u64_le};

// ─── Meta Parser Structures ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct MetaData {
    pub blocks: Vec<MetaBlock>,
    pub root_block_index: i32,
}

#[derive(Debug)]
pub struct MetaBlock {
    pub name_hash: u32,
    pub data_ptr: u64,
    pub length: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Copy, Clone)]
pub struct MetaPointer {
    pub block_id: usize,
    pub offset: usize,
}

impl MetaPointer {
    pub fn from_u64(val: u64) -> Self {
        Self {
            block_id: (val & 0xFFF) as usize,
            offset: ((val >> 12) & 0xFFFFF) as usize,
        }
    }
}

// ─── Structs ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct PedVariationInfo {
    pub has_tex_variations: bool,
    pub has_drawbl_variations: bool,
    pub has_low_lods: bool,
    pub is_super_lod: bool,
    pub avail_comp: [u8; 12],
    pub component_data: Vec<ComponentData>,
}

#[derive(Debug)]
pub struct ComponentData {
    pub num_avail_tex: u8,
    pub drawables: Vec<DrawableData>,
    pub block_id: usize,
    pub offset: usize,
}

#[derive(Debug)]
pub struct DrawableData {
    pub prop_mask: u8,
    pub num_alternatives: u8,
    pub textures: Vec<TextureData>,
    pub block_id: usize,
    pub offset: usize,
}

#[derive(Debug)]
pub struct TextureData {
    pub tex_id: u32,
    pub distribution: u8,
}

/// The 12 component slots, in the order `availComp` indexes them
/// (the reference tool's `MCPVComponentData.ComponentTypeNames`).
pub const PED_COMPONENT_NAMES: [&str; 12] =
    ["head", "berd", "hair", "uppr", "lowr", "hand", "feet", "teef", "accs", "task", "decl", "jbib"];

/// One drawable variant a ped viewer would list for a slot
/// (`PedsForm.PopulateCompCombo`): a drawable of the slot, one of its
/// alternatives and one of its textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PedVariant {
    pub drawable: usize,
    pub alternative: usize,
    /// `None` when the drawable lists no textures.
    pub texture: Option<usize>,
}

impl PedVariationInfo {
    /// The name of slot `slot` (0..12), or `"error"` past the end, as
    /// The reference tool prints it.
    pub fn slot_name(slot: usize) -> &'static str {
        PED_COMPONENT_NAMES.get(slot).copied().unwrap_or("error")
    }

    /// The slot called `name` (`"uppr"`), case-insensitively.
    pub fn slot_index(name: &str) -> Option<usize> {
        PED_COMPONENT_NAMES.iter().position(|n| n.eq_ignore_ascii_case(name.trim()))
    }

    /// The component data of slot `slot`: `availComp[slot]` indexes
    /// `component_data`, 255 (or anything past the end) meaning the ped has
    /// nothing for that slot (`MCPedVariationInfo.GetComponentData`).
    pub fn component(&self, slot: usize) -> Option<&ComponentData> {
        let index = *self.avail_comp.get(slot)? as usize;
        self.component_data.get(index)
    }

    /// Every (drawable, alternative, texture) triple of slot `slot`, in the
    /// order the ped viewer's combo box lists them.
    pub fn variants(&self, slot: usize) -> Vec<PedVariant> {
        let mut out = Vec::new();
        let Some(component) = self.component(slot) else { return out };
        for (drawable, data) in component.drawables.iter().enumerate() {
            for alternative in 0..=data.num_alternatives as usize {
                if data.textures.is_empty() {
                    out.push(PedVariant { drawable, alternative, texture: None });
                } else {
                    for texture in 0..data.textures.len() {
                        out.push(PedVariant { drawable, alternative, texture: Some(texture) });
                    }
                }
            }
        }
        out
    }
}

impl DrawableData {
    /// `(propMask >> 4) & 3`: which of the `_u`/`_r`/`_m` name suffixes the
    /// drawable's file carries.
    pub fn prop_type(&self) -> u8 {
        (self.prop_mask >> 4) & 3
    }

    /// The drawable's name in the ped's `.ydd` (`uppr_001_r`, `hair_000_u_2`
    /// for alternative 2), as `MCPVDrawblData.GetDrawableName` builds it.
    pub fn drawable_name(&self, slot: usize, index: usize, alternative: usize) -> String {
        let mut name = format!("{}_{index:03}_", PedVariationInfo::slot_name(slot));
        name.push_str(match self.prop_type() {
            0 => "u",
            1 => "r",
            2 | 3 => "m",
            _ => "",
        });
        if alternative > 0 {
            name.push_str(&format!("_{alternative}"));
        }
        name
    }

    /// The diffuse texture's name in the ped's `.ytd`
    /// (`uppr_diff_001_a_whi`), as `MCPVDrawblData.GetTextureName` builds it:
    /// a letter per texture index and a race code from the texture's id.
    /// `None` when the drawable lists no textures.
    pub fn texture_name(&self, slot: usize, index: usize, texture: usize) -> Option<String> {
        let tex = self.textures.get(texture)?;
        const ALPHAS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
        let letter = ALPHAS[texture % 26] as char;
        let race = match tex.tex_id {
            0 => "uni",
            1 => "whi",
            2 => "bla",
            3 => "chi",
            4 => "lat",
            5 => "ara",
            8 => "kor",
            10 => "pak",
            _ => "whi",
        };
        Some(format!("{}_diff_{index:03}_{letter}_{race}", PedVariationInfo::slot_name(slot)))
    }
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub fn parse_ymt(data: &[u8]) -> Result<(PedVariationInfo, Vec<u8>, Vec<u8>)> {
    let (system, graphics) = prepare_rsc7(data)?;
    
    // Meta block is 112 bytes long starting at 0
    if system.len() < 112 {
        bail!("System section too small for Meta header");
    }
    
    let root_block_index = i32::from_le_bytes(system[28..32].try_into().unwrap());
    let data_blocks_pointer = u64_le(&system, 48);
    let data_blocks_count = i16::from_le_bytes(system[76..78].try_into().unwrap()) as usize;
    
    let mut blocks = Vec::with_capacity(data_blocks_count);
    
    if data_blocks_count > 0 && data_blocks_pointer != 0 {
        let block_array_offset = (data_blocks_pointer & 0xFFFFFFF) as usize; 
        
        for i in 0..data_blocks_count {
            let offset = block_array_offset + (i * 16);
            let name_hash = u32_le(&system, offset);
            let length = u32_le(&system, offset + 4);
            let data_ptr = u64_le(&system, offset + 8);
            
            let data_offset = (data_ptr & 0xFFFFFFF) as usize;
            
            let mut block_data = Vec::new();
            if data_offset > 0 && data_offset + length as usize <= system.len() {
                block_data = system[data_offset..data_offset + length as usize].to_vec();
            }
            
            blocks.push(MetaBlock {
                name_hash,
                data_ptr,
                length,
                data: block_data
            });
        }
    } else {
        bail!("Invalid Meta header: no data blocks");
    }
    
    let meta = MetaData { blocks, root_block_index };
    
    // Find the CPedVariationInfo block (hash 0x16760659 = 376833625)
    let root_block_opt = meta.blocks.iter().find(|b| b.name_hash == 0x16760659);
    
    let root_block_container = root_block_opt.context("CPedVariationInfo block not found")?;
    let root_block = &root_block_container.data;
    
    if root_block.len() < 112 {
        bail!("Root block too small for CPedVariationInfo");
    }
    
    let has_tex_variations = root_block[0] != 0;
    let has_drawbl_variations = root_block[1] != 0;
    let has_low_lods = root_block[2] != 0;
    let is_super_lod = root_block[3] != 0;
    
    let mut avail_comp = [0u8; 12];
    avail_comp.copy_from_slice(&root_block[4..16]);
    
    let comp_data_ptr = MetaPointer::from_u64(u64_le(root_block, 16));
    let comp_data_count = u16_le(root_block, 24) as usize;
    
    let mut component_data = vec![];
    if comp_data_count > 0 && comp_data_ptr.block_id > 0 {
        let block_idx = comp_data_ptr.block_id - 1; // 1-based index
        let comp_block = meta.blocks.get(block_idx).context("Component block not found")?;
        
        for i in 0..comp_data_count {
            let offset = comp_data_ptr.offset + (i * 24);
            if offset + 24 > comp_block.data.len() { continue; }
            let num_avail_tex = comp_block.data[offset];
            
            let drawables_ptr = MetaPointer::from_u64(u64_le(&comp_block.data, offset + 8));
            let drawables_count = u16_le(&comp_block.data, offset + 16) as usize;
            
            let mut drawables = vec![];
            if drawables_count > 0 && drawables_ptr.block_id > 0 {
                let d_block_idx = drawables_ptr.block_id - 1;
                if let Some(draw_block) = meta.blocks.get(d_block_idx) {
                    for j in 0..drawables_count {
                        let d_offset = drawables_ptr.offset + (j * 48);
                        if d_offset + 48 > draw_block.data.len() { continue; }
                        let prop_mask = draw_block.data[d_offset];
                        let num_alternatives = draw_block.data[d_offset + 1];
                        
                        let tex_data_ptr = MetaPointer::from_u64(u64_le(&draw_block.data, d_offset + 8));
                        let tex_data_count = u16_le(&draw_block.data, d_offset + 16) as usize;
                        
                        let mut textures = vec![];
                        if tex_data_count > 0 && tex_data_ptr.block_id > 0 {
                            let t_block_idx = tex_data_ptr.block_id - 1;
                            if let Some(tex_block) = meta.blocks.get(t_block_idx) {
                                // CPVTextureData is 3 bytes (texId, distribution, padding),
                                // packed at that stride, as the reference tool's struct says.
                                for k in 0..tex_data_count {
                                    let t_offset = tex_data_ptr.offset + (k * 3);
                                    if t_offset + 3 > tex_block.data.len() { continue; }
                                    let tex_id = u32::from(tex_block.data[t_offset]);
                                    let distribution = tex_block.data[t_offset + 1];
                                    textures.push(TextureData { tex_id, distribution });
                                }
                            }
                        }
                        
                        drawables.push(DrawableData { 
                            prop_mask, 
                            num_alternatives, 
                            textures,
                            block_id: d_block_idx,
                            offset: d_offset
                        });
                    }
                }
            }
            
            component_data.push(ComponentData { num_avail_tex, drawables, block_id: block_idx, offset });
        }
    }

    
    Ok((PedVariationInfo {
        has_tex_variations,
        has_drawbl_variations,
        has_low_lods,
        is_super_lod,
        avail_comp,
        component_data,
    }, system, graphics))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PedVariationInfo {
        let tex = |id: u32| TextureData { tex_id: id, distribution: 255 };
        let drawable = |prop_mask: u8, alternatives: u8, textures: Vec<TextureData>| DrawableData {
            prop_mask,
            num_alternatives: alternatives,
            textures,
            block_id: 0,
            offset: 0,
        };
        PedVariationInfo {
            has_tex_variations: true,
            has_drawbl_variations: true,
            has_low_lods: false,
            is_super_lod: false,
            avail_comp: [0, 255, 1, 2, 255, 255, 255, 255, 3, 255, 255, 255],
            component_data: vec![
                ComponentData { num_avail_tex: 3, drawables: vec![drawable(25, 0, vec![tex(1), tex(1), tex(1)])], block_id: 0, offset: 0 },
                ComponentData { num_avail_tex: 2, drawables: vec![drawable(9, 1, vec![tex(0), tex(0)])], block_id: 0, offset: 0 },
                ComponentData { num_avail_tex: 1, drawables: vec![drawable(1, 0, vec![tex(2)]), drawable(33, 0, vec![])], block_id: 0, offset: 0 },
                ComponentData { num_avail_tex: 0, drawables: vec![], block_id: 0, offset: 0 },
            ],
        }
    }

    #[test]
    fn slots_are_named_and_indexed_as_reference_does() {
        assert_eq!(PedVariationInfo::slot_name(3), "uppr");
        assert_eq!(PedVariationInfo::slot_name(11), "jbib");
        assert_eq!(PedVariationInfo::slot_name(12), "error");
        assert_eq!(PedVariationInfo::slot_index("UPPR "), Some(3));
        assert_eq!(PedVariationInfo::slot_index("hat"), None);
    }

    #[test]
    fn components_follow_avail_comp() {
        let info = info();
        assert_eq!(info.component(0).map(|c| c.num_avail_tex), Some(3), "head is component 0");
        assert!(info.component(1).is_none(), "255 means no berd");
        assert_eq!(info.component(2).map(|c| c.num_avail_tex), Some(2), "hair is component 1");
        assert_eq!(info.component(3).map(|c| c.num_avail_tex), Some(1));
        assert_eq!(info.component(8).map(|c| c.drawables.len()), Some(0), "accs exists but lists nothing");
        assert!(info.component(12).is_none());
    }

    #[test]
    fn names_follow_prop_mask_alternative_and_texture_id() {
        let info = info();
        let head = &info.component(0).unwrap().drawables[0];
        assert_eq!(head.prop_type(), 1, "propMask 25 = 0b11001");
        assert_eq!(head.drawable_name(0, 0, 0), "head_000_r");
        assert_eq!(head.texture_name(0, 0, 0).as_deref(), Some("head_diff_000_a_whi"));
        assert_eq!(head.texture_name(0, 0, 2).as_deref(), Some("head_diff_000_c_whi"));
        assert_eq!(head.texture_name(0, 0, 3), None);

        let hair = &info.component(2).unwrap().drawables[0];
        assert_eq!(hair.drawable_name(2, 0, 0), "hair_000_u");
        assert_eq!(hair.drawable_name(2, 0, 1), "hair_000_u_1");
        assert_eq!(hair.texture_name(2, 0, 1).as_deref(), Some("hair_diff_000_b_uni"));

        let uppr = &info.component(3).unwrap().drawables;
        assert_eq!(uppr[0].drawable_name(3, 0, 0), "uppr_000_u");
        assert_eq!(uppr[0].texture_name(3, 0, 0).as_deref(), Some("uppr_diff_000_a_bla"));
        assert_eq!(uppr[1].prop_type(), 2);
        assert_eq!(uppr[1].drawable_name(3, 1, 0), "uppr_001_m");
        assert_eq!(uppr[1].texture_name(3, 1, 0), None);

        let mut other = DrawableData { prop_mask: 0x30, num_alternatives: 0, textures: vec![TextureData { tex_id: 8, distribution: 0 }], block_id: 0, offset: 0 };
        assert_eq!(other.drawable_name(4, 7, 0), "lowr_007_m", "prop type 3 is m too");
        assert_eq!(other.texture_name(4, 7, 0).as_deref(), Some("lowr_diff_007_a_kor"));
        other.textures[0].tex_id = 99;
        assert_eq!(other.texture_name(4, 7, 0).as_deref(), Some("lowr_diff_007_a_whi"), "unknown ids fall back to whi");
    }

    #[test]
    fn variants_list_every_drawable_alternative_and_texture() {
        let info = info();
        let v = |d, a, t: Option<usize>| PedVariant { drawable: d, alternative: a, texture: t };
        assert_eq!(info.variants(0), vec![v(0, 0, Some(0)), v(0, 0, Some(1)), v(0, 0, Some(2))]);
        assert_eq!(info.variants(2), vec![v(0, 0, Some(0)), v(0, 0, Some(1)), v(0, 1, Some(0)), v(0, 1, Some(1))]);
        assert_eq!(info.variants(3), vec![v(0, 0, Some(0)), v(1, 0, None)]);
        assert!(info.variants(1).is_empty() && info.variants(8).is_empty());
    }

    /// The retail `a_m_y_acult_01.ymt` under `RAGE_TEST_PED_DIR`: its head is
    /// `head_000_r` (propMask 25) and the drawable dictionary beside it holds
    /// that hash.
    #[test]
    fn retail_ped_names_match_its_dictionary() {
        let Some(dir) = std::env::var_os("RAGE_TEST_PED_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        let Ok(data) = std::fs::read(dir.join("a_m_y_acult_01.ymt")) else { return };
        let (info, _, _) = parse_ymt(&data).unwrap();
        assert_eq!(info.avail_comp, [0, 255, 1, 2, 3, 255, 255, 255, 4, 255, 255, 255]);
        let head = &info.component(0).unwrap().drawables[0];
        assert_eq!(head.drawable_name(0, 0, 0), "head_000_r");
        assert_eq!(head.texture_name(0, 0, 0).as_deref(), Some("head_diff_000_a_whi"));
        let uppr = &info.component(3).unwrap().drawables[1];
        assert_eq!(uppr.drawable_name(3, 1, 0), "uppr_001_r");
        let hair = &info.component(2).unwrap().drawables[0];
        assert_eq!(hair.texture_name(2, 0, 0).as_deref(), Some("hair_diff_000_a_uni"), "hair texId is 0: 3-byte entries");
        assert_eq!(hair.texture_name(2, 0, 1).as_deref(), Some("hair_diff_000_b_uni"));
        let ydd = crate::ydd::parse_ydd(&std::fs::read(dir.join("a_m_y_acult_01.ydd")).unwrap()).unwrap();
        let hashes: Vec<u32> = ydd.iter().map(|e| e.hash).collect();
        for slot in 0..12 {
            let Some(component) = info.component(slot) else { continue };
            for (index, drawable) in component.drawables.iter().enumerate() {
                let name = drawable.drawable_name(slot, index, 0);
                assert!(hashes.contains(&crate::hash::rage_joaat(&name)), "{name} is not in the dictionary");
            }
        }
    }
}
