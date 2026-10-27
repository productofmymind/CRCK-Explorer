//! `peds.ymt` / `peds.meta`: the ped model list (`CPedModelInfo__InitDataList`),
//! every ped the game can spawn with the names of its animation, expression,
//! capsule and component sets. Ported from the reference tool's `PedsFile.cs`, which
//! reads the same members off the XML form of the file; here the PSO `.ymt`
//! and the XML `.meta` go through the same tree.

use anyhow::Result;

use crate::gtxd::TxdRelationship;
use crate::meta_read::{parse_tree, root_struct, Fields};
use crate::value::MetaStruct;

/// `CPedModelInfo__InitDataList`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PedsMeta {
    pub resident_txd: String,
    pub resident_anims: Vec<String>,
    pub init_datas: Vec<PedInitData>,
    pub txd_relationships: Vec<TxdRelationship>,
    pub multi_txd_relationships: Vec<MultiTxdRelationship>,
}

/// `CMultiTxdRelationship`: one parent dictionary for several children.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultiTxdRelationship {
    pub parent: String,
    pub children: Vec<String>,
}

/// `CPedModelInfo__InitData`, every member `PedsFile.cs` reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PedInitData {
    pub name: String,
    pub props_name: String,
    pub clip_dictionary_name: String,
    pub blend_shape_file_name: String,
    pub expression_set_name: String,
    pub expression_dictionary_name: String,
    pub expression_name: String,
    pub pedtype: String,
    pub movement_clip_set: String,
    pub movement_clip_sets: Vec<String>,
    pub strafe_clip_set: String,
    pub movement_to_strafe_clip_set: String,
    pub injured_strafe_clip_set: String,
    pub full_body_damage_clip_set: String,
    pub additive_damage_clip_set: String,
    pub default_gesture_clip_set: String,
    pub facial_clipset_group_name: String,
    pub default_viseme_clip_set: String,
    pub sidestep_clip_set: String,
    pub pose_matcher_name: String,
    pub pose_matcher_prone_name: String,
    pub getup_set_hash: String,
    pub creature_metadata_name: String,
    pub decision_maker_name: String,
    pub motion_task_data_set_name: String,
    pub default_task_data_set_name: String,
    pub ped_capsule_name: String,
    pub ped_layout_name: String,
    pub ped_component_set_name: String,
    pub ped_component_cloth_name: String,
    pub ped_ik_settings_name: String,
    pub task_data_name: String,
    pub is_streamed_gfx: bool,
    pub ambulance_should_respond_to: bool,
    pub can_ride_bike_with_no_helmet: bool,
    pub can_spawn_in_car: bool,
    pub is_head_blend_ped: bool,
    pub only_bulky_item_variations: bool,
    pub relationship_group: String,
    pub nav_capabilities_name: String,
    pub perception_info: String,
    pub default_brawling_style: String,
    pub default_unarmed_weapon: String,
    pub personality: String,
    pub combat_info: String,
    pub vfx_info_name: String,
    pub ambient_clips_for_flee: String,
    pub radio1: String,
    pub radio2: String,
    pub f_up_offset: f32,
    pub r_up_offset: f32,
    pub f_front_offset: f32,
    pub r_front_offset: f32,
    pub min_activation_impulse: f32,
    pub stubble: f32,
    pub hd_dist: f32,
    pub targeting_threat_modifier: f32,
    pub killed_perception_range_modifer: f32,
    pub sexiness: String,
    pub age: u8,
    pub max_passengers_in_car: u8,
    pub externally_driven_dofs: String,
    pub ped_voice_group: String,
    pub animal_audio_object: String,
    pub ability_type: String,
    pub thermal_behaviour: String,
    pub superlod_type: String,
    pub scenario_pop_streaming_slot: String,
    pub default_spawning_preference: String,
    pub default_remove_range_multiplier: f32,
    pub allow_close_spawning: bool,
}

impl PedInitData {
    fn read(s: &MetaStruct) -> Self {
        PedInitData {
            name: s.text("Name"),
            props_name: s.text("PropsName"),
            clip_dictionary_name: s.text("ClipDictionaryName"),
            blend_shape_file_name: s.text("BlendShapeFileName"),
            expression_set_name: s.text("ExpressionSetName"),
            expression_dictionary_name: s.text("ExpressionDictionaryName"),
            expression_name: s.text("ExpressionName"),
            pedtype: s.text("Pedtype"),
            movement_clip_set: s.text("MovementClipSet"),
            movement_clip_sets: s.strings("MovementClipSets"),
            strafe_clip_set: s.text("StrafeClipSet"),
            movement_to_strafe_clip_set: s.text("MovementToStrafeClipSet"),
            injured_strafe_clip_set: s.text("InjuredStrafeClipSet"),
            full_body_damage_clip_set: s.text("FullBodyDamageClipSet"),
            additive_damage_clip_set: s.text("AdditiveDamageClipSet"),
            default_gesture_clip_set: s.text("DefaultGestureClipSet"),
            facial_clipset_group_name: s.text("FacialClipsetGroupName"),
            default_viseme_clip_set: s.text("DefaultVisemeClipSet"),
            sidestep_clip_set: s.text("SidestepClipSet"),
            pose_matcher_name: s.text("PoseMatcherName"),
            pose_matcher_prone_name: s.text("PoseMatcherProneName"),
            getup_set_hash: s.text("GetupSetHash"),
            creature_metadata_name: s.text("CreatureMetadataName"),
            decision_maker_name: s.text("DecisionMakerName"),
            motion_task_data_set_name: s.text("MotionTaskDataSetName"),
            default_task_data_set_name: s.text("DefaultTaskDataSetName"),
            ped_capsule_name: s.text("PedCapsuleName"),
            ped_layout_name: s.text("PedLayoutName"),
            ped_component_set_name: s.text("PedComponentSetName"),
            ped_component_cloth_name: s.text("PedComponentClothName"),
            ped_ik_settings_name: s.text("PedIKSettingsName"),
            task_data_name: s.text("TaskDataName"),
            is_streamed_gfx: s.bool("IsStreamedGfx"),
            ambulance_should_respond_to: s.bool("AmbulanceShouldRespondTo"),
            can_ride_bike_with_no_helmet: s.bool("CanRideBikeWithNoHelmet"),
            can_spawn_in_car: s.bool("CanSpawnInCar"),
            is_head_blend_ped: s.bool("IsHeadBlendPed"),
            only_bulky_item_variations: s.bool("bOnlyBulkyItemVariations"),
            relationship_group: s.text("RelationshipGroup"),
            nav_capabilities_name: s.text("NavCapabilitiesName"),
            perception_info: s.text("PerceptionInfo"),
            default_brawling_style: s.text("DefaultBrawlingStyle"),
            default_unarmed_weapon: s.text("DefaultUnarmedWeapon"),
            personality: s.text("Personality"),
            combat_info: s.text("CombatInfo"),
            vfx_info_name: s.text("VfxInfoName"),
            ambient_clips_for_flee: s.text("AmbientClipsForFlee"),
            radio1: s.text("Radio1"),
            radio2: s.text("Radio2"),
            f_up_offset: s.f32("FUpOffset"),
            r_up_offset: s.f32("RUpOffset"),
            f_front_offset: s.f32("FFrontOffset"),
            r_front_offset: s.f32("RFrontOffset"),
            min_activation_impulse: s.f32("MinActivationImpulse"),
            stubble: s.f32("Stubble"),
            hd_dist: s.f32("HDDist"),
            targeting_threat_modifier: s.f32("TargetingThreatModifier"),
            killed_perception_range_modifer: s.f32("KilledPerceptionRangeModifer"),
            sexiness: s.text("Sexiness"),
            age: s.u8("Age"),
            max_passengers_in_car: s.u8("MaxPassengersInCar"),
            externally_driven_dofs: s.text("ExternallyDrivenDOFs"),
            ped_voice_group: s.text("PedVoiceGroup"),
            animal_audio_object: s.text("AnimalAudioObject"),
            ability_type: s.text("AbilityType"),
            thermal_behaviour: s.text("ThermalBehaviour"),
            superlod_type: s.text("SuperlodType"),
            scenario_pop_streaming_slot: s.text("ScenarioPopStreamingSlot"),
            default_spawning_preference: s.text("DefaultSpawningPreference"),
            default_remove_range_multiplier: s.f32("DefaultRemoveRangeMultiplier"),
            allow_close_spawning: s.bool("AllowCloseSpawning"),
        }
    }
}

/// Parses a `peds.ymt` (PSO) or `peds.meta` (XML).
pub fn parse_peds_meta(data: &[u8]) -> Result<PedsMeta> {
    let root = parse_tree(data)?;
    let list = root_struct(&root, "CPedModelInfo__InitDataList")?;
    Ok(PedsMeta {
        resident_txd: list.text("residentTxd"),
        resident_anims: list.strings("residentAnims"),
        init_datas: list.structs("InitDatas").into_iter().map(PedInitData::read).collect(),
        txd_relationships: list
            .structs("txdRelationships")
            .into_iter()
            .map(|r| TxdRelationship { parent: r.text("parent"), child: r.text("child") })
            .collect(),
        multi_txd_relationships: list
            .structs("multiTxdRelationships")
            .into_iter()
            .map(|r| MultiTxdRelationship { parent: r.text("parent"), children: r.strings("children") })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CPedModelInfo__InitDataList>
  <residentTxd>comp_peds_generic</residentTxd>
  <residentAnims><Item>move_m@generic</Item></residentAnims>
  <InitDatas>
    <Item>
      <Name>CS_LesterCrest_3</Name>
      <PropsName>CS_LesterCrest_3_p</PropsName>
      <ClipDictionaryName>move_m@generic</ClipDictionaryName>
      <ExpressionSetName />
      <MovementClipSets><Item>a</Item><Item>b</Item></MovementClipSets>
      <IsStreamedGfx value="true" />
      <CanSpawnInCar value="true" />
      <bOnlyBulkyItemVariations value="false" />
      <RFrontOffset value="0.147000" />
      <HDDist value="3" />
      <Age value="7" />
      <ThermalBehaviour>TB_WARM</ThermalBehaviour>
      <DefaultRemoveRangeMultiplier value="1.000000" />
    </Item>
  </InitDatas>
  <txdRelationships><Item><parent>p</parent><child>c</child></Item></txdRelationships>
  <multiTxdRelationships><Item><parent>comp_peds_helmets_moped</parent><children><Item>A_F_M_EastSA_02_p</Item><Item>A_F_M_SouCent_01_p</Item></children></Item></multiTxdRelationships>
</CPedModelInfo__InitDataList>"#;

    #[test]
    fn reads_the_xml_form() {
        let peds = parse_peds_meta(XML.as_bytes()).unwrap();
        assert_eq!(peds.resident_txd, "comp_peds_generic");
        assert_eq!(peds.resident_anims, vec!["move_m@generic"]);
        assert_eq!(peds.init_datas.len(), 1);
        let p = &peds.init_datas[0];
        assert_eq!(p.name, "CS_LesterCrest_3");
        assert_eq!(p.props_name, "CS_LesterCrest_3_p");
        assert_eq!(p.movement_clip_sets, vec!["a", "b"]);
        assert!(p.is_streamed_gfx && p.can_spawn_in_car && !p.only_bulky_item_variations);
        assert_eq!(p.r_front_offset, 0.147);
        assert_eq!(p.hd_dist, 3.0);
        assert_eq!(p.age, 7);
        assert_eq!(p.thermal_behaviour, "TB_WARM");
        assert_eq!(p.expression_set_name, "");
        assert_eq!(peds.txd_relationships, vec![TxdRelationship { child: "c".into(), parent: "p".into() }]);
        assert_eq!(peds.multi_txd_relationships[0].children.len(), 2);
    }

    #[test]
    fn wrong_root_is_an_error() {
        assert!(parse_peds_meta(b"<CVehicleModelInfoVarGlobal/>").is_err());
    }

    /// The retail `peds.ymt` (PSO), when `RAGE_TEST_META_DIR` points at a folder
    /// holding one, reads every ped with a name and the resident dictionary.
    #[test]
    fn reads_the_retail_pso_form() {
        let Some(dir) = std::env::var_os("RAGE_TEST_META_DIR") else { return };
        let path = std::path::Path::new(&dir).join("peds.ymt");
        let Ok(data) = std::fs::read(&path) else { return };
        let peds = parse_peds_meta(&data).unwrap();
        assert_eq!(peds.resident_txd, "comp_peds_generic");
        assert!(peds.init_datas.len() > 600, "{}", peds.init_datas.len());
        assert!(peds.init_datas.iter().all(|p| !p.name.is_empty()));
        let player = peds.init_datas.iter().find(|p| p.name == "Player_One").expect("Player_One");
        assert_eq!(player.props_name, "Player_One_p");
        assert!(player.is_streamed_gfx);
        assert_eq!(player.hd_dist, 3.0);
        assert_eq!(player.radio1, "RADIO_GENRE_MODERN_HIPHOP");
        assert!(!peds.multi_txd_relationships.is_empty());
    }
}
