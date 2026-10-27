//! `vehicles.meta`: the vehicle model list (`CVehicleModelInfo__InitDataList`),
//! one entry per model naming its texture dictionary, handling, display name,
//! cameras, layout, LOD distances, class and flags. Ported from the reference tool's
//! `VehiclesFile.cs` (`VehicleInitData`), member for member. The game ships
//! this file as XML only, which is all the reference tool reads; the tree path here
//! would take a PSO form just the same. The texture-dictionary relationships
//! the same file declares are also what [`crate::gtxd`] reads.

use anyhow::Result;

use crate::gtxd::TxdRelationship;
use crate::math::Vec3;
use crate::meta_read::{parse_tree, root_struct, Fields};
use crate::value::MetaStruct;

/// `CVehicleModelInfo__InitDataList`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehiclesMeta {
    pub resident_txd: String,
    pub init_datas: Vec<VehicleInitData>,
    /// Child -> parent texture dictionary names, first child wins
    /// (the reference tool keeps them in a dictionary).
    pub txd_relationships: Vec<TxdRelationship>,
}

/// `CVehicleModelInfo__InitData`, every member `VehicleInitData.Load` reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleInitData {
    pub model_name: String,
    pub txd_name: String,
    pub handling_id: String,
    pub game_name: String,
    pub vehicle_make_name: String,
    pub expression_dict_name: String,
    pub expression_name: String,
    pub anim_conv_roof_dict_name: String,
    pub anim_conv_roof_name: String,
    pub anim_conv_roof_windows_affected: String,
    pub ptfx_asset_name: String,
    pub audio_name_hash: String,
    pub layout: String,
    pub cover_bound_offsets: String,
    pub explosion_info: String,
    pub scenario_layout: String,
    pub camera_name: String,
    pub aim_camera_name: String,
    pub bonnet_camera_name: String,
    pub pov_camera_name: String,
    pub first_person_drive_by_ik_offset: Vec3,
    pub first_person_drive_by_unarmed_ik_offset: Vec3,
    pub first_person_projectile_drive_by_ik_offset: Vec3,
    pub first_person_projectile_drive_by_passenger_ik_offset: Vec3,
    pub first_person_drive_by_right_passenger_ik_offset: Vec3,
    pub first_person_drive_by_right_passenger_unarmed_ik_offset: Vec3,
    pub first_person_mobile_phone_offset: Vec3,
    pub first_person_passenger_mobile_phone_offset: Vec3,
    pub pov_camera_offset: Vec3,
    pub pov_camera_vertical_adjustment_for_roll_cage: Vec3,
    pub pov_passenger_camera_offset: Vec3,
    pub pov_rear_passenger_camera_offset: Vec3,
    pub vfx_info_name: String,
    pub should_use_cinematic_view_mode: bool,
    pub should_camera_transition_on_climb_up_down: bool,
    pub should_camera_ignore_exiting: bool,
    pub allow_pretend_occupants: bool,
    pub allow_joyriding: bool,
    pub allow_sunday_driving: bool,
    pub allow_body_color_mapping: bool,
    pub wheel_scale: f32,
    pub wheel_scale_rear: f32,
    pub dirt_level_min: f32,
    pub dirt_level_max: f32,
    pub env_eff_scale_min: f32,
    pub env_eff_scale_max: f32,
    pub env_eff_scale_min2: f32,
    pub env_eff_scale_max2: f32,
    pub damage_map_scale: f32,
    pub damage_offset_scale: f32,
    /// `0xAARRGGBB` as written (`<diffuseTint value="0x00FFFFFF" />`).
    pub diffuse_tint: u32,
    pub steer_wheel_mult: f32,
    pub hd_texture_dist: f32,
    pub lod_distances: Vec<f32>,
    pub min_seat_height: f32,
    pub identical_model_spawn_distance: f32,
    pub max_num_of_same_color: i32,
    pub default_body_health: f32,
    pub pretend_occupants_scale: f32,
    pub visible_spawn_dist_scale: f32,
    pub tracker_path_width: f32,
    pub weapon_force_mult: f32,
    pub frequency: f32,
    pub swankness: String,
    pub max_num: i32,
    pub flags: Vec<String>,
    pub vehicle_type: String,
    pub plate_type: String,
    pub dashboard_type: String,
    pub vehicle_class: String,
    pub wheel_type: String,
    pub trailers: Vec<String>,
    pub additional_trailers: Vec<String>,
    pub drivers: Vec<VehicleDriver>,
    pub extra_includes: Vec<String>,
    pub doors_with_collision_when_closed: Vec<String>,
    pub driveable_doors: Vec<String>,
    pub bumpers_need_to_collide_with_map: bool,
    pub needs_rope_texture: bool,
    pub required_extras: Vec<String>,
    pub rewards: Vec<String>,
    pub cinematic_part_camera: Vec<String>,
    pub nm_brace_override_set: String,
    pub buoyancy_sphere_offset: Vec3,
    pub buoyancy_sphere_size_scale: f32,
    pub override_ragdoll_threshold: Option<VehicleOverrideRagdollThreshold>,
    pub first_person_driveby_data: Vec<String>,
}

/// `CVehicleModelInfo__CVehicleOverrideRagdollThreshold`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleOverrideRagdollThreshold {
    pub min_component: i32,
    pub max_component: i32,
    pub threshold_mult: f32,
}

/// One `drivers` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleDriver {
    pub driver_name: String,
    pub npc_name: String,
}

impl VehicleInitData {
    fn read(s: &MetaStruct) -> Self {
        VehicleInitData {
            model_name: s.text("modelName"),
            txd_name: s.text("txdName"),
            handling_id: s.text("handlingId"),
            game_name: s.text("gameName"),
            vehicle_make_name: s.text("vehicleMakeName"),
            expression_dict_name: s.text("expressionDictName"),
            expression_name: s.text("expressionName"),
            anim_conv_roof_dict_name: s.text("animConvRoofDictName"),
            anim_conv_roof_name: s.text("animConvRoofName"),
            anim_conv_roof_windows_affected: s.text("animConvRoofWindowsAffected"),
            ptfx_asset_name: s.text("ptfxAssetName"),
            audio_name_hash: s.text("audioNameHash"),
            layout: s.text("layout"),
            cover_bound_offsets: s.text("coverBoundOffsets"),
            explosion_info: s.text("explosionInfo"),
            scenario_layout: s.text("scenarioLayout"),
            camera_name: s.text("cameraName"),
            aim_camera_name: s.text("aimCameraName"),
            bonnet_camera_name: s.text("bonnetCameraName"),
            pov_camera_name: s.text("povCameraName"),
            first_person_drive_by_ik_offset: s.vec3("FirstPersonDriveByIKOffset"),
            first_person_drive_by_unarmed_ik_offset: s.vec3("FirstPersonDriveByUnarmedIKOffset"),
            first_person_projectile_drive_by_ik_offset: s.vec3("FirstPersonProjectileDriveByIKOffset"),
            first_person_projectile_drive_by_passenger_ik_offset: s.vec3("FirstPersonProjectileDriveByPassengerIKOffset"),
            first_person_drive_by_right_passenger_ik_offset: s.vec3("FirstPersonDriveByRightPassengerIKOffset"),
            first_person_drive_by_right_passenger_unarmed_ik_offset: s.vec3("FirstPersonDriveByRightPassengerUnarmedIKOffset"),
            first_person_mobile_phone_offset: s.vec3("FirstPersonMobilePhoneOffset"),
            first_person_passenger_mobile_phone_offset: s.vec3("FirstPersonPassengerMobilePhoneOffset"),
            pov_camera_offset: s.vec3("PovCameraOffset"),
            pov_camera_vertical_adjustment_for_roll_cage: s.vec3("PovCameraVerticalAdjustmentForRollCage"),
            pov_passenger_camera_offset: s.vec3("PovPassengerCameraOffset"),
            pov_rear_passenger_camera_offset: s.vec3("PovRearPassengerCameraOffset"),
            vfx_info_name: s.text("vfxInfoName"),
            should_use_cinematic_view_mode: s.bool("shouldUseCinematicViewMode"),
            should_camera_transition_on_climb_up_down: s.bool("shouldCameraTransitionOnClimbUpDown"),
            should_camera_ignore_exiting: s.bool("shouldCameraIgnoreExiting"),
            allow_pretend_occupants: s.bool("AllowPretendOccupants"),
            allow_joyriding: s.bool("AllowJoyriding"),
            allow_sunday_driving: s.bool("AllowSundayDriving"),
            allow_body_color_mapping: s.bool("AllowBodyColorMapping"),
            wheel_scale: s.f32("wheelScale"),
            wheel_scale_rear: s.f32("wheelScaleRear"),
            dirt_level_min: s.f32("dirtLevelMin"),
            dirt_level_max: s.f32("dirtLevelMax"),
            env_eff_scale_min: s.f32("envEffScaleMin"),
            env_eff_scale_max: s.f32("envEffScaleMax"),
            env_eff_scale_min2: s.f32("envEffScaleMin2"),
            env_eff_scale_max2: s.f32("envEffScaleMax2"),
            damage_map_scale: s.f32("damageMapScale"),
            damage_offset_scale: s.f32("damageOffsetScale"),
            diffuse_tint: s.u32("diffuseTint"),
            steer_wheel_mult: s.f32("steerWheelMult"),
            hd_texture_dist: s.f32("HDTextureDist"),
            lod_distances: s.floats("lodDistances"),
            min_seat_height: s.f32("minSeatHeight"),
            identical_model_spawn_distance: s.f32("identicalModelSpawnDistance"),
            max_num_of_same_color: s.i32("maxNumOfSameColor"),
            default_body_health: s.f32("defaultBodyHealth"),
            pretend_occupants_scale: s.f32("pretendOccupantsScale"),
            visible_spawn_dist_scale: s.f32("visibleSpawnDistScale"),
            tracker_path_width: s.f32("trackerPathWidth"),
            weapon_force_mult: s.f32("weaponForceMult"),
            frequency: s.f32("frequency"),
            swankness: s.text("swankness"),
            max_num: s.i32("maxNum"),
            flags: s.words("flags"),
            vehicle_type: s.text("type"),
            plate_type: s.text("plateType"),
            dashboard_type: s.text("dashboardType"),
            vehicle_class: s.text("vehicleClass"),
            wheel_type: s.text("wheelType"),
            trailers: s.strings("trailers"),
            additional_trailers: s.strings("additionalTrailers"),
            drivers: s
                .structs("drivers")
                .into_iter()
                .map(|d| VehicleDriver { driver_name: d.text("driverName"), npc_name: d.text("npcName") })
                .collect(),
            extra_includes: s.strings("extraIncludes"),
            doors_with_collision_when_closed: s.strings("doorsWithCollisionWhenClosed"),
            driveable_doors: s.strings("driveableDoors"),
            bumpers_need_to_collide_with_map: s.bool("bumpersNeedToCollideWithMap"),
            needs_rope_texture: s.bool("needsRopeTexture"),
            required_extras: s.words("requiredExtras"),
            rewards: s.strings("rewards"),
            cinematic_part_camera: s.strings("cinematicPartCamera"),
            nm_brace_override_set: s.text("NmBraceOverrideSet"),
            buoyancy_sphere_offset: s.vec3("buoyancySphereOffset"),
            buoyancy_sphere_size_scale: s.f32("buoyancySphereSizeScale"),
            override_ragdoll_threshold: s.child("pOverrideRagdollThreshold").map(|t| VehicleOverrideRagdollThreshold {
                min_component: t.i32("MinComponent"),
                max_component: t.i32("MaxComponent"),
                threshold_mult: t.f32("ThresholdMult"),
            }),
            first_person_driveby_data: s.strings("firstPersonDrivebyData"),
        }
    }
}

/// Parses a `vehicles.meta`.
pub fn parse_vehicles_meta(data: &[u8]) -> Result<VehiclesMeta> {
    let root = parse_tree(data)?;
    let list = root_struct(&root, "CVehicleModelInfo__InitDataList")?;
    let mut txd_relationships: Vec<TxdRelationship> = Vec::new();
    for r in list.structs("txdRelationships") {
        let (parent, child) = (r.text("parent"), r.text("child"));
        if parent.is_empty() || child.is_empty() || txd_relationships.iter().any(|t| t.child == child) {
            continue;
        }
        txd_relationships.push(TxdRelationship { child, parent });
    }
    Ok(VehiclesMeta {
        resident_txd: list.text("residentTxd"),
        init_datas: list.structs("InitDatas").into_iter().map(VehicleInitData::read).collect(),
        txd_relationships,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfo__InitDataList>
  <residentTxd>vehshare</residentTxd>
  <residentAnims />
  <InitDatas>
    <Item>
      <modelName>minitank</modelName>
      <txdName>minitank</txdName>
      <handlingId>MINITANK</handlingId>
      <gameName>MINITANK</gameName>
      <vehicleMakeName />
      <animConvRoofDictName>null</animConvRoofDictName>
      <layout>LAYOUT_RCTANK</layout>
      <FirstPersonDriveByIKOffset x="0.020000" y="-0.065000" z="-0.050000" />
      <PovCameraVerticalAdjustmentForRollCage value="0.000000" />
      <shouldUseCinematicViewMode value="true" />
      <AllowBodyColorMapping value="false" />
      <wheelScale value="0.202300" />
      <diffuseTint value="0x00FFFFFF" />
      <lodDistances content="float_array">
        15.000000
        30.000000
        60.000000
        120.000000
        500.000000
        500.000000
      </lodDistances>
      <maxNumOfSameColor value="10" />
      <frequency value="100" />
      <swankness>SWANKNESS_2</swankness>
      <maxNum value="5" />
      <flags>FLAG_NO_BOOT FLAG_IS_TANK</flags>
      <type>VEHICLE_TYPE_CAR</type>
      <plateType>VPT_NONE</plateType>
      <vehicleClass>VC_MILITARY</vehicleClass>
      <wheelType>VWT_SPORT</wheelType>
      <trailers />
      <drivers>
        <Item><driverName>s_m_y_cop_01</driverName><npcName /></Item>
      </drivers>
      <requiredExtras>EXTRA_1 EXTRA_2</requiredExtras>
      <cinematicPartCamera>
        <Item>WHEEL_FRONT_RIGHT_CAMERA</Item>
        <Item>WHEEL_FRONT_LEFT_CAMERA</Item>
      </cinematicPartCamera>
      <buoyancySphereOffset x="0.000000" y="1.000000" z="0.000000" />
      <pOverrideRagdollThreshold type="CVehicleModelInfo__CVehicleOverrideRagdollThreshold">
        <MinComponent value="3" />
        <MaxComponent value="7" />
        <ThresholdMult value="2.500000" />
      </pOverrideRagdollThreshold>
      <firstPersonDrivebyData><Item>STD_IMPALER2_FRONT_LEFT</Item></firstPersonDrivebyData>
    </Item>
    <Item>
      <modelName>adder</modelName>
      <txdName>adder</txdName>
      <pOverrideRagdollThreshold type="NULL" />
    </Item>
  </InitDatas>
  <txdRelationships>
    <Item><parent>vehshare</parent><child>minitank</child></Item>
    <Item><parent>other</parent><child>minitank</child></Item>
    <Item><parent /><child>adder</child></Item>
  </txdRelationships>
</CVehicleModelInfo__InitDataList>"#;

    #[test]
    fn reads_the_xml_form() {
        let v = parse_vehicles_meta(XML.as_bytes()).unwrap();
        assert_eq!(v.resident_txd, "vehshare");
        assert_eq!(v.init_datas.len(), 2);
        let t = &v.init_datas[0];
        assert_eq!((t.model_name.as_str(), t.txd_name.as_str(), t.game_name.as_str()), ("minitank", "minitank", "MINITANK"));
        assert_eq!(t.vehicle_make_name, "");
        assert_eq!(t.first_person_drive_by_ik_offset, Vec3::new(0.02, -0.065, -0.05));
        assert!(t.should_use_cinematic_view_mode && !t.allow_body_color_mapping);
        assert_eq!(t.wheel_scale, 0.2023);
        assert_eq!(t.diffuse_tint, 0x00FF_FFFF);
        assert_eq!(t.lod_distances, vec![15.0, 30.0, 60.0, 120.0, 500.0, 500.0]);
        assert_eq!((t.max_num_of_same_color, t.max_num, t.frequency), (10, 5, 100.0));
        assert_eq!(t.flags, vec!["FLAG_NO_BOOT", "FLAG_IS_TANK"]);
        assert_eq!((t.vehicle_type.as_str(), t.vehicle_class.as_str(), t.wheel_type.as_str()), ("VEHICLE_TYPE_CAR", "VC_MILITARY", "VWT_SPORT"));
        assert!(t.trailers.is_empty());
        assert_eq!(t.drivers, vec![VehicleDriver { driver_name: "s_m_y_cop_01".into(), npc_name: String::new() }]);
        assert_eq!(t.required_extras, vec!["EXTRA_1", "EXTRA_2"]);
        assert_eq!(t.cinematic_part_camera.len(), 2);
        assert_eq!(t.buoyancy_sphere_offset, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(
            t.override_ragdoll_threshold,
            Some(VehicleOverrideRagdollThreshold { min_component: 3, max_component: 7, threshold_mult: 2.5 })
        );
        assert_eq!(t.first_person_driveby_data, vec!["STD_IMPALER2_FRONT_LEFT"]);
        assert_eq!(v.init_datas[1].override_ragdoll_threshold, None);
        assert_eq!(v.txd_relationships, vec![TxdRelationship { child: "minitank".into(), parent: "vehshare".into() }], "first child wins, empty parents skipped");
    }

    #[test]
    fn reads_the_retail_file() {
        let Some(dir) = std::env::var_os("RAGE_TEST_META_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("vehicles.meta")) else { return };
        let v = parse_vehicles_meta(&data).unwrap();
        assert_eq!(v.resident_txd, "vehshare");
        let police = v.init_datas.iter().find(|d| d.model_name == "police").expect("police");
        assert_eq!(police.txd_name, "police");
        assert_eq!((police.handling_id.as_str(), police.game_name.as_str()), ("POLICE", "POLICE"));
        assert_eq!(police.vehicle_class, "VC_EMERGENCY");
        assert_eq!(police.lod_distances.len(), 6);
        assert!(police.flags.iter().any(|f| f.starts_with("FLAG_")));
        assert!(!v.txd_relationships.is_empty());
    }
}
