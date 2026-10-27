//! `carcols.ymt` / `carcols.meta`: the vehicle colour and tuning catalogue
//! (`CVehicleModelInfoVarGlobal`) — the colour list carvariations indexes into,
//! metallic and window settings, plate textures, light and siren settings,
//! mod kits with their liveries, wheel menus and xenon colours. Ported from
//! The reference tool's `CarColsFile.cs`, class for class and member for member; the
//! game's `update.rpf/x64/data/carcols.ymt` is PSO and a DLC's
//! `common/data/carcols.meta` is XML, and both read through one tree.

use anyhow::Result;

use crate::math::{Vec2, Vec4};
use crate::meta_enum;
use crate::meta_read::{parse_tree, root_struct, Fields};
use crate::value::MetaStruct;

/// `CVehicleModelInfoVarGlobal`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CarCols {
    pub plates: Option<VehiclePlates>,
    pub colors: Vec<VehicleModelColor>,
    pub metallic_settings: Vec<VehicleMetallicSetting>,
    pub window_colors: Vec<VehicleWindowColor>,
    pub lights: Vec<VehicleLightSettings>,
    pub sirens: Vec<SirenSettings>,
    pub kits: Vec<VehicleKit>,
    /// One list per wheel type (`VWT_SPORT`, `VWT_MUSCLE`, …), in the file's order.
    pub wheels: Vec<Vec<VehicleWheel>>,
    pub global_variation_data: Option<GlobalVariationData>,
    pub xenon_light_colors: Vec<VehicleXenonLightColor>,
}

/// `VehiclePlates` (the reference tool's `CVehicleModelInfoVarGlobal_465922034`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehiclePlates {
    pub textures: Vec<PlateTextureSet>,
    pub default_texture_index: i32,
    pub numeric_offset: u8,
    pub alphabetic_offset: u8,
    pub space_offset: u8,
    pub random_char_offset: u8,
    pub num_random_char: u8,
}

/// `CVehicleModelInfoPlateTextureSet`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlateTextureSet {
    pub texture_set_name: u32,
    pub diffuse_map_name: u32,
    pub normal_map_name: u32,
    pub font_extents: Vec4,
    pub max_letters_on_plate: Vec2,
    pub font_color: u32,
    pub font_outline_color: u32,
    pub is_font_outline_enabled: bool,
    pub font_outline_min_max_depth: Vec2,
}

/// `CVehicleModelColor`: one paint, `color` as `0xAARRGGBB`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleModelColor {
    pub color: u32,
    pub metallic_id: MetallicId,
    pub audio_color: AudioColor,
    pub audio_prefix: AudioPrefix,
    pub audio_color_hash: u32,
    pub audio_prefix_hash: u32,
    pub color_name: String,
}

impl VehicleModelColor {
    /// The paint as `[r, g, b]`.
    pub fn rgb(&self) -> [u8; 3] {
        [(self.color >> 16) as u8, (self.color >> 8) as u8, self.color as u8]
    }
}

/// `CVehicleMetallicSetting`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleMetallicSetting {
    pub spec_int: f32,
    pub spec_falloff: f32,
    pub spec_fresnel: f32,
}

/// `CVehicleWindowColor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleWindowColor {
    pub color: u32,
    pub name: u32,
}

/// `vehicleLightSettings`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleLightSettings {
    pub id: u8,
    pub indicator: Option<VehicleLight>,
    pub rear_indicator_corona: Option<VehicleCorona>,
    pub front_indicator_corona: Option<VehicleCorona>,
    pub tail_light: Option<VehicleLight>,
    pub tail_light_corona: Option<VehicleCorona>,
    pub tail_light_middle_corona: Option<VehicleCorona>,
    pub head_light: Option<VehicleLight>,
    pub head_light_corona: Option<VehicleCorona>,
    pub reversing_light: Option<VehicleLight>,
    pub reversing_light_corona: Option<VehicleCorona>,
    pub name: String,
}

/// `vehicleLight`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleLight {
    pub intensity: f32,
    pub falloff_max: f32,
    pub falloff_exponent: f32,
    pub inner_cone_angle: f32,
    pub outer_cone_angle: f32,
    pub emmissive_boost: bool,
    pub color: u32,
    pub texture_name: u32,
    pub mirror_texture: bool,
}

/// `vehicleCorona`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleCorona {
    pub size: f32,
    pub size_far: f32,
    pub intensity: f32,
    pub intensity_far: f32,
    pub color: u32,
    pub num_coronas: u8,
    pub dist_between_coronas: u8,
    pub dist_between_coronas_far: u8,
    pub x_rotation: f32,
    pub y_rotation: f32,
    pub z_rotation: f32,
    pub z_bias: f32,
    pub pull_corona_in: bool,
}

/// `sirenSettings`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SirenSettings {
    pub id: u8,
    pub name: String,
    pub time_multiplier: f32,
    pub light_falloff_max: f32,
    pub light_falloff_exponent: f32,
    pub light_inner_cone_angle: f32,
    pub light_outer_cone_angle: f32,
    pub light_offset: f32,
    pub texture_name: u32,
    pub sequencer_bpm: u32,
    pub left_head_light: Option<SirenSequencer>,
    pub right_head_light: Option<SirenSequencer>,
    pub left_tail_light: Option<SirenSequencer>,
    pub right_tail_light: Option<SirenSequencer>,
    pub left_head_light_multiples: u8,
    pub right_head_light_multiples: u8,
    pub left_tail_light_multiples: u8,
    pub right_tail_light_multiples: u8,
    pub use_real_lights: bool,
    pub sirens: Vec<SirenLight>,
}

/// A siren's headlight/taillight sequencer (`sirenSettings_188820339`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SirenSequencer {
    pub sequencer: u32,
}

/// `sirenLight`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SirenLight {
    pub rotation: Option<SirenMotion>,
    pub flashiness: Option<SirenMotion>,
    pub corona: Option<SirenCorona>,
    pub color: u32,
    pub intensity: f32,
    pub light_group: u8,
    pub rotate: bool,
    pub scale: bool,
    pub scale_factor: u8,
    pub flash: bool,
    pub light: bool,
    pub spot_light: bool,
    pub cast_shadows: bool,
}

/// A siren light's rotation or flash pattern (`sirenLight_1356743507`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SirenMotion {
    pub delta: f32,
    pub start: f32,
    pub speed: f32,
    pub sequencer: u32,
    pub multiples: u8,
    pub direction: bool,
    pub sync_to_bpm: bool,
}

/// `sirenCorona`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SirenCorona {
    pub intensity: f32,
    pub size: f32,
    pub pull: f32,
    pub face_camera: bool,
}

/// `CVehicleKit`: a mod kit, with the liveries its `VMT_LIVERY_MOD` slot offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleKit {
    pub kit_name: u32,
    pub id: u16,
    pub kit_type: ModKitType,
    pub visible_mods: Vec<VehicleModVisible>,
    pub link_mods: Vec<VehicleModLink>,
    pub stat_mods: Vec<VehicleModStat>,
    pub slot_names: Vec<VehicleKitSlotName>,
    pub livery_names: Vec<u32>,
    pub livery2_names: Vec<u32>,
}

/// `CVehicleModVisible`: a mod that swaps in a model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleModVisible {
    pub model_name: u32,
    pub mod_shop_label: String,
    pub linked_models: Vec<u32>,
    pub turn_off_bones: Vec<VehicleModBone>,
    pub mod_type: VehicleModType,
    pub bone: VehicleModBone,
    pub collision_bone: VehicleModBone,
    pub camera_pos: VehicleModCameraPos,
    pub audio_apply: AudioApply,
    pub weight: u8,
    pub turn_off_extra: bool,
    pub disable_bonnet_camera: bool,
    pub allow_bonnet_slide: bool,
    pub weapon_slot: i8,
    pub weapon_slot_secondary: i8,
    pub disable_projectile_driveby: bool,
    pub disable_driveby: bool,
    pub disable_driveby_seat: i32,
    pub disable_driveby_seat_secondary: i32,
}

/// `audioApply` is a float in the file; kept as its bits so the structs
/// around it can be compared exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioApply(pub u32);

impl AudioApply {
    pub fn value(self) -> f32 {
        f32::from_bits(self.0)
    }
}

/// `CVehicleModLink`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleModLink {
    pub model_name: u32,
    pub bone: VehicleModBone,
    pub turn_off_extra: bool,
}

/// `CVehicleModStat`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleModStat {
    pub identifier: u32,
    pub modifier: u32,
    pub audio_apply: AudioApply,
    pub weight: u8,
    pub mod_type: VehicleModType,
}

/// A `slotNames` entry (`CVehicleKit_427606548`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleKitSlotName {
    pub slot: VehicleModType,
    pub name: String,
}

/// `CVehicleWheel`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleWheel {
    pub wheel_name: u32,
    pub wheel_variation: u32,
    pub mod_shop_label: String,
    pub rim_radius: f32,
    pub rear: bool,
}

/// `GlobalVariationData` (`CVehicleModelInfoVarGlobal_3062246906`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GlobalVariationData {
    pub xenon_light_color: u32,
    pub xenon_corona_color: u32,
    pub xenon_light_intensity_modifier: f32,
    pub xenon_corona_intensity_modifier: f32,
}

/// `CVehicleXenonLightColor`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleXenonLightColor {
    pub light_color: u32,
    pub corona_color: u32,
    pub light_intensity_modifier: f32,
    pub corona_intensity_modifier: f32,
}

meta_enum! {
    /// `metallicID` (the reference tool's `CVehicleModelColor_360458334`).
    pub enum MetallicId {
        none = -1,
        EVehicleModelColorMetallic_normal = 0,
        EVehicleModelColorMetallic_1 = 1,
        EVehicleModelColorMetallic_2 = 2,
        EVehicleModelColorMetallic_3 = 3,
        EVehicleModelColorMetallic_4 = 4,
        EVehicleModelColorMetallic_5 = 5,
        EVehicleModelColorMetallic_6 = 6,
        EVehicleModelColorMetallic_7 = 7,
        EVehicleModelColorMetallic_8 = 8,
        EVehicleModelColorMetallic_9 = 9,
    }
}

meta_enum! {
    /// `audioColor` (`CVehicleModelColor_544262540`).
    pub enum AudioColor {
        POLICE_SCANNER_COLOUR_black = 0,
        POLICE_SCANNER_COLOUR_blue = 1,
        POLICE_SCANNER_COLOUR_brown = 2,
        POLICE_SCANNER_COLOUR_beige = 3,
        POLICE_SCANNER_COLOUR_graphite = 4,
        POLICE_SCANNER_COLOUR_green = 5,
        POLICE_SCANNER_COLOUR_grey = 6,
        POLICE_SCANNER_COLOUR_orange = 7,
        POLICE_SCANNER_COLOUR_pink = 8,
        POLICE_SCANNER_COLOUR_red = 9,
        POLICE_SCANNER_COLOUR_silver = 10,
        POLICE_SCANNER_COLOUR_white = 11,
        POLICE_SCANNER_COLOUR_yellow = 12,
    }
}

meta_enum! {
    /// `audioPrefix` (`CVehicleModelColor_2065815796`).
    pub enum AudioPrefix {
        none = 0,
        POLICE_SCANNER_PREFIX_bright = 1,
        POLICE_SCANNER_PREFIX_light = 2,
        POLICE_SCANNER_PREFIX_dark = 3,
    }
}

meta_enum! {
    /// `eModKitType`.
    pub enum ModKitType {
        MKT_STANDARD = 0,
        MKT_SPORT = 1,
        MKT_SUV = 2,
        MKT_SPECIAL = 3,
    }
}

meta_enum! {
    /// `eVehicleModType`.
    pub enum VehicleModType {
        VMT_SPOILER = 0,
        VMT_BUMPER_F = 1,
        VMT_BUMPER_R = 2,
        VMT_SKIRT = 3,
        VMT_EXHAUST = 4,
        VMT_CHASSIS = 5,
        VMT_GRILL = 6,
        VMT_BONNET = 7,
        VMT_WING_L = 8,
        VMT_WING_R = 9,
        VMT_ROOF = 10,
        VMT_PLTHOLDER = 11,
        VMT_PLTVANITY = 12,
        VMT_INTERIOR1 = 13,
        VMT_INTERIOR2 = 14,
        VMT_INTERIOR3 = 15,
        VMT_INTERIOR4 = 16,
        VMT_INTERIOR5 = 17,
        VMT_SEATS = 18,
        VMT_STEERING = 19,
        VMT_KNOB = 20,
        VMT_PLAQUE = 21,
        VMT_ICE = 22,
        VMT_TRUNK = 23,
        VMT_HYDRO = 24,
        VMT_ENGINEBAY1 = 25,
        VMT_ENGINEBAY2 = 26,
        VMT_ENGINEBAY3 = 27,
        VMT_CHASSIS2 = 28,
        VMT_CHASSIS3 = 29,
        VMT_CHASSIS4 = 30,
        VMT_CHASSIS5 = 31,
        VMT_DOOR_L = 32,
        VMT_DOOR_R = 33,
        VMT_LIVERY_MOD = 34,
        Unk_3409280882 = 35,
        VMT_ENGINE = 36,
        VMT_BRAKES = 37,
        VMT_GEARBOX = 38,
        VMT_HORN = 39,
        VMT_SUSPENSION = 40,
        VMT_ARMOUR = 41,
        Unk_3278520444 = 42,
        VMT_TURBO = 43,
        Unk_1675686396 = 44,
        VMT_TYRE_SMOKE = 45,
        VMT_HYDRAULICS = 46,
        VMT_XENON_LIGHTS = 47,
        VMT_WHEELS = 48,
        VMT_WHEELS_REAR_OR_HYDRAULICS = 49,
    }
}

meta_enum! {
    /// `eVehicleModCameraPos`.
    pub enum VehicleModCameraPos {
        VMCP_DEFAULT = 0,
        VMCP_FRONT = 1,
        VMCP_FRONT_LEFT = 2,
        VMCP_FRONT_RIGHT = 3,
        VMCP_REAR = 4,
        VMCP_REAR_LEFT = 5,
        VMCP_REAR_RIGHT = 6,
        VMCP_LEFT = 7,
        VMCP_RIGHT = 8,
        VMCP_TOP = 9,
        VMCP_BOTTOM = 10,
    }
}

meta_enum! {
    /// A vehicle mod bone (`CVehicleMod_3635907608`), as the reference tool lists them.
    pub enum VehicleModBone {
        none = -1,
        chassis = 0,
        bodyshell = 48,
        bumper_f = 49,
        bumper_r = 50,
        wing_rf = 51,
        wing_lf = 52,
        bonnet = 53,
        boot = 54,
        exhaust = 56,
        exhaust_2 = 57,
        exhaust_3 = 58,
        exhaust_4 = 59,
        exhaust_5 = 60,
        exhaust_6 = 61,
        exhaust_7 = 62,
        exhaust_8 = 63,
        exhaust_9 = 64,
        exhaust_10 = 65,
        exhaust_11 = 66,
        exhaust_12 = 67,
        exhaust_13 = 68,
        exhaust_14 = 69,
        exhaust_15 = 70,
        exhaust_16 = 71,
        extra_1 = 401,
        extra_2 = 402,
        extra_3 = 403,
        extra_4 = 404,
        extra_5 = 405,
        extra_6 = 406,
        extra_7 = 407,
        extra_8 = 408,
        extra_9 = 409,
        extra_10 = 410,
        extra_11 = 411,
        extra_12 = 412,
        extra_13 = 413,
        extra_14 = 414,
        break_extra_1 = 417,
        break_extra_2 = 418,
        break_extra_3 = 419,
        break_extra_4 = 420,
        break_extra_5 = 421,
        break_extra_6 = 422,
        break_extra_7 = 423,
        break_extra_8 = 424,
        break_extra_9 = 425,
        break_extra_10 = 426,
        mod_col_1 = 427,
        mod_col_2 = 428,
        mod_col_3 = 429,
        mod_col_4 = 430,
        mod_col_5 = 431,
        mod_col_6 = 432,
        mod_col_7 = 433,
        mod_col_8 = 434,
        mod_col_9 = 435,
        mod_col_10 = 436,
        mod_col_11 = 437,
        mod_col_12 = 438,
        mod_col_13 = 439,
        mod_col_14 = 440,
        mod_col_15 = 441,
        mod_col_16 = 442,
        misc_a = 369,
        misc_b = 370,
        misc_c = 371,
        misc_d = 372,
        misc_e = 373,
        misc_f = 374,
        misc_g = 375,
        misc_h = 376,
        misc_i = 377,
        misc_j = 378,
        misc_k = 379,
        misc_l = 380,
        misc_m = 381,
        misc_n = 382,
        misc_o = 383,
        misc_p = 384,
        misc_q = 385,
        misc_r = 386,
        misc_s = 387,
        misc_t = 388,
        misc_u = 389,
        misc_v = 390,
        misc_w = 391,
        misc_x = 392,
        misc_y = 393,
        misc_z = 394,
        misc_1 = 395,
        misc_2 = 396,
        handlebars = 79,
        steeringwheel = 80,
        swingarm = 29,
        forks_u = 21,
        forks_l = 22,
        headlight_l = 91,
        headlight_r = 92,
        indicator_lr = 97,
        indicator_lf = 95,
        indicator_rr = 98,
        indicator_rf = 96,
        taillight_l = 93,
        taillight_r = 94,
        window_lf = 42,
        window_rf = 43,
        window_rr = 45,
        window_lr = 44,
        window_lm = 46,
        window_rm = 47,
        hub_lf = 30,
        hub_rf = 31,
        windscreen_r = 41,
        neon_l = 104,
        neon_r = 105,
        neon_f = 106,
        neon_b = 107,
        door_dside_f = 3,
        door_dside_r = 4,
        door_pside_f = 5,
        door_pside_r = 6,
        bobble_head = 361,
        bobble_base = 362,
        bobble_hand = 363,
        engineblock = 364,
        mod_a = 474,
        mod_b = 475,
        mod_c = 476,
        mod_d = 477,
        mod_e = 478,
        mod_f = 479,
        mod_g = 480,
        mod_h = 481,
        mod_i = 482,
        mod_j = 483,
        mod_k = 484,
        mod_l = 485,
        mod_m = 486,
        mod_n = 487,
        mod_o = 488,
        mod_p = 489,
        mod_q = 490,
        mod_r = 491,
        mod_s = 492,
        mod_t = 493,
        mod_u = 494,
        mod_v = 495,
        mod_w = 496,
        mod_x = 497,
        mod_y = 498,
        mod_z = 499,
        mod_aa = 500,
        mod_ab = 501,
        mod_ac = 502,
        mod_ad = 503,
        mod_ae = 504,
        mod_af = 505,
        mod_ag = 506,
        mod_ah = 507,
        mod_ai = 508,
        mod_aj = 509,
        mod_ak = 510,
        turret_a1 = 511,
        turret_a2 = 512,
        turret_a3 = 513,
        turret_a4 = 514,
        turret_b1 = 524,
        turret_b2 = 525,
        turret_b3 = 526,
        turret_b4 = 527,
        rblade_1mod = 560,
        rblade_1fast = 561,
        rblade_2mod = 562,
        rblade_2fast = 563,
        rblade_3mod = 564,
        rblade_3fast = 565,
        fblade_1mod = 566,
        fblade_1fast = 567,
        fblade_2mod = 568,
        fblade_2fast = 569,
        fblade_3mod = 570,
        fblade_3fast = 571,
        Unk_1086719913 = 572,
        Unk_3237490897 = 573,
        Unk_3375838140 = 574,
        Unk_2381840182 = 575,
        Unk_3607058940 = 576,
        Unk_3607058940_again = 577,
        Unk_1208798824 = 578,
        Unk_303656220 = 579,
        Unk_660207018 = 580,
        spike_1mod = 581,
        Unk_3045655218 = 582,
        Unk_2017296145 = 583,
        spike_2mod = 584,
        Unk_1122332083 = 585,
        Unk_1123212214 = 586,
        spike_3mod = 587,
        Unk_4011591561 = 588,
        Unk_2320654166 = 589,
        scoop_1mod = 590,
        scoop_2mod = 591,
        scoop_3mod = 592,
    }
}

fn light(s: &MetaStruct) -> VehicleLight {
    VehicleLight {
        intensity: s.f32("intensity"),
        falloff_max: s.f32("falloffMax"),
        falloff_exponent: s.f32("falloffExponent"),
        inner_cone_angle: s.f32("innerConeAngle"),
        outer_cone_angle: s.f32("outerConeAngle"),
        emmissive_boost: s.bool("emmissiveBoost"),
        color: s.u32("color"),
        texture_name: s.hash("textureName"),
        mirror_texture: s.bool("mirrorTexture"),
    }
}

fn corona(s: &MetaStruct) -> VehicleCorona {
    VehicleCorona {
        size: s.f32("size"),
        size_far: s.f32("size_far"),
        intensity: s.f32("intensity"),
        intensity_far: s.f32("intensity_far"),
        color: s.u32("color"),
        num_coronas: s.u8("numCoronas"),
        dist_between_coronas: s.u8("distBetweenCoronas"),
        dist_between_coronas_far: s.u8("distBetweenCoronas_far"),
        x_rotation: s.f32("xRotation"),
        y_rotation: s.f32("yRotation"),
        z_rotation: s.f32("zRotation"),
        z_bias: s.f32("zBias"),
        pull_corona_in: s.bool("pullCoronaIn"),
    }
}

fn light_settings(s: &MetaStruct) -> VehicleLightSettings {
    VehicleLightSettings {
        id: s.u8("id"),
        indicator: s.child("indicator").map(light),
        rear_indicator_corona: s.child("rearIndicatorCorona").map(corona),
        front_indicator_corona: s.child("frontIndicatorCorona").map(corona),
        tail_light: s.child("tailLight").map(light),
        tail_light_corona: s.child("tailLightCorona").map(corona),
        tail_light_middle_corona: s.child("tailLightMiddleCorona").map(corona),
        head_light: s.child("headLight").map(light),
        head_light_corona: s.child("headLightCorona").map(corona),
        reversing_light: s.child("reversingLight").map(light),
        reversing_light_corona: s.child("reversingLightCorona").map(corona),
        name: s.text("name"),
    }
}

fn sequencer(s: &MetaStruct) -> SirenSequencer {
    SirenSequencer { sequencer: s.u32("sequencer") }
}

fn motion(s: &MetaStruct) -> SirenMotion {
    SirenMotion {
        delta: s.f32("delta"),
        start: s.f32("start"),
        speed: s.f32("speed"),
        sequencer: s.u32("sequencer"),
        multiples: s.u8("multiples"),
        direction: s.bool("direction"),
        sync_to_bpm: s.bool("syncToBpm"),
    }
}

fn siren_light(s: &MetaStruct) -> SirenLight {
    SirenLight {
        rotation: s.child("rotation").map(motion),
        flashiness: s.child("flashiness").map(motion),
        corona: s.child("corona").map(|c| SirenCorona {
            intensity: c.f32("intensity"),
            size: c.f32("size"),
            pull: c.f32("pull"),
            face_camera: c.bool("faceCamera"),
        }),
        color: s.u32("color"),
        intensity: s.f32("intensity"),
        light_group: s.u8("lightGroup"),
        rotate: s.bool("rotate"),
        scale: s.bool("scale"),
        scale_factor: s.u8("scaleFactor"),
        flash: s.bool("flash"),
        light: s.bool("light"),
        spot_light: s.bool("spotLight"),
        cast_shadows: s.bool("castShadows"),
    }
}

fn siren_settings(s: &MetaStruct) -> SirenSettings {
    SirenSettings {
        id: s.u8("id"),
        name: s.text("name"),
        time_multiplier: s.f32("timeMultiplier"),
        light_falloff_max: s.f32("lightFalloffMax"),
        light_falloff_exponent: s.f32("lightFalloffExponent"),
        light_inner_cone_angle: s.f32("lightInnerConeAngle"),
        light_outer_cone_angle: s.f32("lightOuterConeAngle"),
        light_offset: s.f32("lightOffset"),
        texture_name: s.hash("textureName"),
        sequencer_bpm: s.u32("sequencerBpm"),
        left_head_light: s.child("leftHeadLight").map(sequencer),
        right_head_light: s.child("rightHeadLight").map(sequencer),
        left_tail_light: s.child("leftTailLight").map(sequencer),
        right_tail_light: s.child("rightTailLight").map(sequencer),
        left_head_light_multiples: s.u8("leftHeadLightMultiples"),
        right_head_light_multiples: s.u8("rightHeadLightMultiples"),
        left_tail_light_multiples: s.u8("leftTailLightMultiples"),
        right_tail_light_multiples: s.u8("rightTailLightMultiples"),
        use_real_lights: s.bool("useRealLights"),
        sirens: s.structs("sirens").into_iter().map(siren_light).collect(),
    }
}

fn audio_apply(s: &MetaStruct) -> AudioApply {
    AudioApply(s.f32("audioApply").to_bits())
}

fn kit(s: &MetaStruct) -> VehicleKit {
    VehicleKit {
        kit_name: s.hash("kitName"),
        id: s.u16("id"),
        kit_type: s.enum_of("kitType"),
        visible_mods: s
            .structs("visibleMods")
            .into_iter()
            .map(|m| VehicleModVisible {
                model_name: m.hash("modelName"),
                mod_shop_label: m.text("modShopLabel"),
                linked_models: m.hashes("linkedModels"),
                turn_off_bones: m.words("turnOffBones").iter().map(|b| bone_by_name(b)).collect(),
                mod_type: m.enum_of("type"),
                bone: m.enum_of("bone"),
                collision_bone: m.enum_of("collisionBone"),
                camera_pos: m.enum_of("cameraPos"),
                audio_apply: audio_apply(m),
                weight: m.u8("weight"),
                turn_off_extra: m.bool("turnOffExtra"),
                disable_bonnet_camera: m.bool("disableBonnetCamera"),
                allow_bonnet_slide: m.bool("allowBonnetSlide"),
                weapon_slot: m.i8("weaponSlot"),
                weapon_slot_secondary: m.i8("weaponSlotSecondary"),
                disable_projectile_driveby: m.bool("disableProjectileDriveby"),
                disable_driveby: m.bool("disableDriveby"),
                disable_driveby_seat: m.i32("disableDrivebySeat"),
                disable_driveby_seat_secondary: m.i32("disableDrivebySeatSecondary"),
            })
            .collect(),
        link_mods: s
            .structs("linkMods")
            .into_iter()
            .map(|m| VehicleModLink { model_name: m.hash("modelName"), bone: m.enum_of("bone"), turn_off_extra: m.bool("turnOffExtra") })
            .collect(),
        stat_mods: s
            .structs("statMods")
            .into_iter()
            .map(|m| VehicleModStat {
                identifier: m.hash("identifier"),
                modifier: m.u32("modifier"),
                audio_apply: audio_apply(m),
                weight: m.u8("weight"),
                mod_type: m.enum_of("type"),
            })
            .collect(),
        slot_names: s
            .structs("slotNames")
            .into_iter()
            .map(|n| VehicleKitSlotName { slot: n.enum_of("slot"), name: n.text("name") })
            .collect(),
        livery_names: s.hashes("liveryNames"),
        livery2_names: s.hashes("livery2Names"),
    }
}

/// A bone from an `<Item>` of `turnOffBones`: its name, or its number.
fn bone_by_name(text: &str) -> VehicleModBone {
    use crate::meta_read::MetaEnum;
    VehicleModBone::from_name(text.trim()).unwrap_or_else(|| VehicleModBone::from_value(text.trim().parse().unwrap_or(0)))
}

/// Parses a `carcols.ymt` (PSO) or `carcols.meta` (XML).
pub fn parse_carcols(data: &[u8]) -> Result<CarCols> {
    let root = parse_tree(data)?;
    let s = root_struct(&root, "CVehicleModelInfoVarGlobal")?;
    Ok(CarCols {
        plates: s.child("VehiclePlates").map(|p| VehiclePlates {
            textures: p
                .structs("Textures")
                .into_iter()
                .map(|t| PlateTextureSet {
                    texture_set_name: t.hash("TextureSetName"),
                    diffuse_map_name: t.hash("DiffuseMapName"),
                    normal_map_name: t.hash("NormalMapName"),
                    font_extents: t.vec4("FontExtents"),
                    max_letters_on_plate: t.vec2("MaxLettersOnPlate"),
                    font_color: t.u32("FontColor"),
                    font_outline_color: t.u32("FontOutlineColor"),
                    is_font_outline_enabled: t.bool("IsFontOutlineEnabled"),
                    font_outline_min_max_depth: t.vec2("FontOutlineMinMaxDepth"),
                })
                .collect(),
            default_texture_index: p.i32("DefaultTextureIndex"),
            numeric_offset: p.u8("NumericOffset"),
            alphabetic_offset: p.u8("AlphabeticOffset"),
            space_offset: p.u8("SpaceOffset"),
            random_char_offset: p.u8("RandomCharOffset"),
            num_random_char: p.u8("NumRandomChar"),
        }),
        colors: s
            .structs("Colors")
            .into_iter()
            .map(|c| VehicleModelColor {
                color: c.u32("color"),
                metallic_id: c.enum_of("metallicID"),
                audio_color: c.enum_of("audioColor"),
                audio_prefix: c.enum_of("audioPrefix"),
                audio_color_hash: c.u32("audioColorHash"),
                audio_prefix_hash: c.u32("audioPrefixHash"),
                color_name: c.text("colorName"),
            })
            .collect(),
        metallic_settings: s
            .structs("MetallicSettings")
            .into_iter()
            .map(|m| VehicleMetallicSetting { spec_int: m.f32("specInt"), spec_falloff: m.f32("specFalloff"), spec_fresnel: m.f32("specFresnel") })
            .collect(),
        window_colors: s
            .structs("WindowColors")
            .into_iter()
            .map(|w| VehicleWindowColor { color: w.u32("color"), name: w.hash("name") })
            .collect(),
        lights: s.structs("Lights").into_iter().map(light_settings).collect(),
        sirens: s.structs("Sirens").into_iter().map(siren_settings).collect(),
        kits: s.structs("Kits").into_iter().map(kit).collect(),
        wheels: s
            .items("Wheels")
            .iter()
            .map(|list| {
                list.items()
                    .iter()
                    .filter_map(|w| w.as_struct())
                    .map(|w| VehicleWheel {
                        wheel_name: w.hash("wheelName"),
                        wheel_variation: w.hash("wheelVariation"),
                        mod_shop_label: w.text("modShopLabel"),
                        rim_radius: w.f32("rimRadius"),
                        rear: w.bool("rear"),
                    })
                    .collect()
            })
            .collect(),
        global_variation_data: s.child("GlobalVariationData").map(|g| GlobalVariationData {
            xenon_light_color: g.u32("xenonLightColor"),
            xenon_corona_color: g.u32("xenonCoronaColor"),
            xenon_light_intensity_modifier: g.f32("xenonLightIntensityModifier"),
            xenon_corona_intensity_modifier: g.f32("xenonCoronaIntensityModifier"),
        }),
        xenon_light_colors: s
            .structs("XenonLightColors")
            .into_iter()
            .map(|x| VehicleXenonLightColor {
                light_color: x.u32("lightColor"),
                corona_color: x.u32("coronaColor"),
                light_intensity_modifier: x.f32("lightIntensityModifier"),
                corona_intensity_modifier: x.f32("coronaIntensityModifier"),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::rage_joaat;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfoVarGlobal>
  <Colors>
    <Item>
      <color value="0xFF0D0D0D" />
      <metallicID>EVehicleModelColorMetallic_1</metallicID>
      <audioColor>POLICE_SCANNER_COLOUR_black</audioColor>
      <audioPrefix>none</audioPrefix>
      <audioColorHash value="-444925803" />
      <audioPrefixHash value="0" />
      <colorName> 0 Metallic Black				</colorName>
    </Item>
    <Item>
      <color value="4287317267" />
      <metallicID>EVehicleModelColorMetallic_normal</metallicID>
      <audioColor>POLICE_SCANNER_COLOUR_red</audioColor>
      <audioPrefix>POLICE_SCANNER_PREFIX_dark</audioPrefix>
      <colorName>Dark Red</colorName>
    </Item>
  </Colors>
  <MetallicSettings><Item><specInt value="0.8"/><specFalloff value="510"/><specFresnel value="0.96"/></Item></MetallicSettings>
  <WindowColors><Item><color value="83886079"/><name>NONE</name></Item></WindowColors>
  <Lights>
    <Item>
      <id value="0" />
      <indicator>
        <intensity value="0.375" /><falloffMax value="2.5" /><falloffExponent value="8" /><innerConeAngle value="30" /><outerConeAngle value="80" />
        <emmissiveBoost value="false" /><color value="0xFFFF8000" /><textureName>VehicleLight_car_standardmodern</textureName><mirrorTexture value="true" />
      </indicator>
      <headLightCorona>
        <size value="0.25" /><size_far value="0.5" /><intensity value="2" /><intensity_far value="1" /><color value="0xFFFFFFFF" />
        <numCoronas value="1" /><distBetweenCoronas value="128" /><distBetweenCoronas_far value="255" /><xRotation value="0" /><yRotation value="0" /><zRotation value="0" /><zBias value="0.25" /><pullCoronaIn value="false" />
      </headLightCorona>
      <name>0_default</name>
    </Item>
  </Lights>
  <Sirens>
    <Item>
      <id value="7" />
      <name>7_police</name>
      <timeMultiplier value="1" />
      <textureName>VehicleLight_sirenlight</textureName>
      <sequencerBpm value="200" />
      <leftHeadLight><sequencer value="0" /></leftHeadLight>
      <leftHeadLightMultiples value="1" />
      <useRealLights value="true" />
      <sirens>
        <Item>
          <rotation><delta value="0" /><start value="0" /><speed value="1" /><sequencer value="4294967295" /><multiples value="1" /><direction value="false" /><syncToBpm value="true" /></rotation>
          <corona><intensity value="8.4" /><size value="0.5" /><pull value="0.15" /><faceCamera value="true" /></corona>
          <color value="0xFFFF0000" />
          <intensity value="2.5" />
          <lightGroup value="1" />
          <rotate value="true" />
          <flash value="true" />
          <light value="true" />
          <castShadows value="true" />
        </Item>
      </sirens>
    </Item>
  </Sirens>
  <Kits>
    <Item>
      <kitName>414_yosemite2_modkit</kitName>
      <id value="414" />
      <kitType>MKT_SPECIAL</kitType>
      <visibleMods>
        <Item>
          <modelName>yose2_exha</modelName>
          <modShopLabel>MNU_EXH1</modShopLabel>
          <linkedModels><Item>yose2_exha_b</Item></linkedModels>
          <turnOffBones><Item>misc_g</Item><Item>extra_1</Item></turnOffBones>
          <type>VMT_EXHAUST</type>
          <bone>chassis</bone>
          <collisionBone>chassis</collisionBone>
          <cameraPos>VMCP_REAR</cameraPos>
          <audioApply value="1.000000" />
          <weight value="20" />
          <turnOffExtra value="false" />
          <disableBonnetCamera value="false" />
          <allowBonnetSlide value="true" />
          <weaponSlot value="-1" />
        </Item>
      </visibleMods>
      <linkMods><Item><modelName>yose2_link</modelName><bone>misc_a</bone><turnOffExtra value="true" /></Item></linkMods>
      <statMods><Item><identifier>ENGINE_1</identifier><modifier value="25" /><audioApply value="1.000000" /><weight value="0" /><type>VMT_ENGINE</type></Item></statMods>
      <slotNames><Item><slot>VMT_CHASSIS2</slot><name>RIM_LIGHTS</name></Item></slotNames>
      <liveryNames>
        <Item>YOSE2_LIV1</Item>
        <Item>YOSE2_LIV2</Item>
      </liveryNames>
      <livery2Names />
    </Item>
  </Kits>
  <Wheels>
    <Item>
      <Item><wheelName>wheel_sport_01</wheelName><wheelVariation /><modShopLabel>WHEEL_SPORT_01</modShopLabel><rimRadius value="0.25" /><rear value="false" /></Item>
    </Item>
    <Item />
  </Wheels>
  <GlobalVariationData>
    <xenonLightColor value="4280907263" /><xenonCoronaColor value="4281364223" /><xenonLightIntensityModifier value="1.5" /><xenonCoronaIntensityModifier value="1.5" />
  </GlobalVariationData>
  <XenonLightColors><Item><lightColor value="4292796159" /><coronaColor value="4292796159" /><lightIntensityModifier value="1.5" /><coronaIntensityModifier value="1.5" /></Item></XenonLightColors>
</CVehicleModelInfoVarGlobal>"#;

    #[test]
    fn reads_the_xml_form() {
        let c = parse_carcols(XML.as_bytes()).unwrap();
        assert_eq!(c.colors.len(), 2);
        let black = &c.colors[0];
        assert_eq!(black.color, 0xFF0D_0D0D);
        assert_eq!(black.rgb(), [0x0D, 0x0D, 0x0D]);
        assert_eq!(black.metallic_id, MetallicId::EVehicleModelColorMetallic_1);
        assert_eq!(black.audio_color, AudioColor::POLICE_SCANNER_COLOUR_black);
        assert_eq!(black.audio_prefix, AudioPrefix::none);
        assert_eq!(black.audio_color_hash, (-444_925_803_i32) as u32, "the reference tool casts the int attribute");
        assert_eq!(black.color_name, "0 Metallic Black", "XML text is trimmed on the way in");
        let red = &c.colors[1];
        assert_eq!(red.rgb(), [0x8B, 0x45, 0x13], "4287317267 is 0xFF8B4513");
        assert_eq!(red.audio_prefix, AudioPrefix::POLICE_SCANNER_PREFIX_dark);
        assert_eq!(c.metallic_settings[0].spec_falloff, 510.0);
        assert_eq!(c.window_colors[0].name, rage_joaat("NONE"));

        let l = &c.lights[0];
        assert_eq!(l.name, "0_default");
        let ind = l.indicator.as_ref().unwrap();
        assert_eq!((ind.intensity, ind.color, ind.mirror_texture), (0.375, 0xFFFF_8000, true));
        assert_eq!(ind.texture_name, rage_joaat("VehicleLight_car_standardmodern"));
        assert_eq!(l.head_light_corona.as_ref().unwrap().dist_between_coronas_far, 255);
        assert!(l.tail_light.is_none());

        let s = &c.sirens[0];
        assert_eq!((s.id, s.name.as_str(), s.sequencer_bpm), (7, "7_police", 200));
        assert_eq!(s.left_head_light, Some(SirenSequencer { sequencer: 0 }));
        assert!(s.right_head_light.is_none() && s.use_real_lights);
        let sl = &s.sirens[0];
        assert_eq!(sl.rotation.as_ref().unwrap().sequencer, u32::MAX);
        assert_eq!(sl.corona.as_ref().unwrap().pull, 0.15);
        assert!(sl.rotate && sl.flash && sl.light && !sl.spot_light && sl.cast_shadows);
        assert_eq!((sl.color, sl.intensity, sl.light_group), (0xFFFF_0000, 2.5, 1));

        let k = &c.kits[0];
        assert_eq!((k.kit_name, k.id, k.kit_type), (rage_joaat("414_yosemite2_modkit"), 414, ModKitType::MKT_SPECIAL));
        let m = &k.visible_mods[0];
        assert_eq!(m.model_name, rage_joaat("yose2_exha"));
        assert_eq!(m.linked_models, vec![rage_joaat("yose2_exha_b")]);
        assert_eq!(m.turn_off_bones, vec![VehicleModBone::misc_g, VehicleModBone::extra_1]);
        assert_eq!((m.mod_type, m.bone, m.camera_pos), (VehicleModType::VMT_EXHAUST, VehicleModBone::chassis, VehicleModCameraPos::VMCP_REAR));
        assert_eq!((m.audio_apply.value(), m.weight, m.allow_bonnet_slide, m.weapon_slot), (1.0, 20, true, -1));
        assert_eq!(k.link_mods[0], VehicleModLink { model_name: rage_joaat("yose2_link"), bone: VehicleModBone::misc_a, turn_off_extra: true });
        assert_eq!((k.stat_mods[0].identifier, k.stat_mods[0].modifier, k.stat_mods[0].mod_type), (rage_joaat("ENGINE_1"), 25, VehicleModType::VMT_ENGINE));
        assert_eq!(k.slot_names[0], VehicleKitSlotName { slot: VehicleModType::VMT_CHASSIS2, name: "RIM_LIGHTS".into() });
        assert_eq!(k.livery_names, vec![rage_joaat("YOSE2_LIV1"), rage_joaat("YOSE2_LIV2")]);
        assert!(k.livery2_names.is_empty());

        assert_eq!(c.wheels.len(), 2);
        assert_eq!(c.wheels[0][0].wheel_name, rage_joaat("wheel_sport_01"));
        assert_eq!(c.wheels[0][0].rim_radius, 0.25);
        assert!(c.wheels[1].is_empty());
        assert_eq!(c.global_variation_data.as_ref().unwrap().xenon_light_color, 4_280_907_263);
        assert_eq!(c.xenon_light_colors[0].light_intensity_modifier, 1.5);
        assert!(c.plates.is_none());
    }

    #[test]
    fn unknown_enum_values_are_kept() {
        use crate::meta_read::MetaEnum;
        assert_eq!(VehicleModBone::from_value(9999), VehicleModBone::Unknown(9999));
        assert_eq!(VehicleModBone::from_name("bonnet"), Some(VehicleModBone::bonnet));
        assert_eq!(VehicleModBone::bonnet.value(), 53);
        assert_eq!(VehicleModType::from_value(34), VehicleModType::VMT_LIVERY_MOD);
        assert_eq!(MetallicId::from_value(-1), MetallicId::none);
        assert_eq!(bone_by_name(" 375 "), VehicleModBone::misc_g);
    }

    #[test]
    fn reads_the_retail_pso_form() {
        let Some(dir) = std::env::var_os("RAGE_TEST_META_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("carcols.ymt")) else { return };
        let c = parse_carcols(&data).unwrap();
        assert!(c.colors.len() > 150, "{}", c.colors.len());
        assert_eq!(c.colors[0].color, 4_278_716_424);
        assert_eq!(c.colors[0].metallic_id, MetallicId::EVehicleModelColorMetallic_1);
        assert_eq!(c.colors[0].audio_color, AudioColor::POLICE_SCANNER_COLOUR_black);
        assert!(c.colors[0].color_name.contains("Metallic Black"), "{:?}", c.colors[0].color_name);
        assert_eq!(c.colors[1].audio_color, AudioColor::POLICE_SCANNER_COLOUR_graphite);
        let plates = c.plates.as_ref().expect("plates");
        assert_eq!(plates.textures[0].diffuse_map_name, rage_joaat("plate01"));
        assert_eq!(plates.textures[0].font_extents, Vec4::new(0.08, 0.355, 0.92, 0.816));
        assert_eq!(c.lights[0].id, 0);
        assert!(c.lights[0].indicator.is_some());
        assert_eq!(c.sirens[0].name, "0_unused");
        assert!(!c.kits.is_empty());
        assert!(c.kits.iter().any(|k| k.kit_type == ModKitType::MKT_STANDARD));
        assert!(c.kits.iter().any(|k| k.visible_mods.iter().any(|m| m.bone != VehicleModBone::none)));
        assert!(c.wheels.len() >= 10);
        assert!(c.global_variation_data.is_some());
        assert!(!c.xenon_light_colors.is_empty());
    }
}
