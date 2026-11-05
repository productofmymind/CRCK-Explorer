//! The LOD-light generator: the lights each placed model carries (`LightAttributes`) become a
//! pair of maps, `<name>_lodlights.ymap` (direction, falloff, flags, hash, cone angles, corona per light) whose parent is
//! `<name>_distantlights.ymap` (position and packed colour per light, the street-light count and a size category).
//!
//! Known approximations: bone transforms of skinned models are not applied (a light on a bone is placed as if the bone were at the model's
//! origin), the light hash is computed from the archetype's own box (the game's also takes the drawable's and the collision's), and fragments
//! (`.yft`) and drawable dictionaries are not read for lights.
use crate::ymap_doc::{ExtentsMode, YmapDocument};
use anyhow::{Context, Result};
use glam::{Quat, Vec3};
pub use rage_formats::drawable_lights::{parse_ydr_lights, DrawableLight};
use rage_formats::rage_joaat;

#[derive(Debug, Clone, PartialEq)]
pub struct LodLight {
    pub position: [f32; 3],
    /// packed intensity, red, green, blue (`i << 24 | r << 16 | g << 8 | b`)
    pub colour: u32,
    pub direction: [f32; 3],
    pub falloff: f32,
    pub falloff_exponent: f32,
    pub time_and_state_flags: u32,
    pub hash: u32,
    pub cone_inner: u8,
    pub cone_outer_or_extent: u8,
    pub corona: u8,
    pub street: bool,
}

fn rotl(x: u32, k: u32) -> u32 {
    x.rotate_left(k)
}

fn rotr(x: u32, k: u32) -> u32 {
    x.rotate_right(k)
}

/// Bob Jenkins' `hashword`-style hash the game uses to tell lights apart: seven integers, the entity's box in tenths of a metre and the
/// light's place in its model.
pub fn compute_light_hash(ints: &[u32]) -> u32 {
    let n = ints.len();
    let mut v3 = n as i64;
    let v5i = 0xDEAD_BEEFu32.wrapping_add(4u32.wrapping_mul(n as u32));
    let (mut v5, mut v6, mut v7) = (v5i, v5i, v5i);
    let mut c = 0usize;
    let rounds = (n.saturating_sub(4)) / 3 + 1;
    for _ in 0..rounds {
        let v9 = ints[c + 2].wrapping_add(v5);
        let v10 = ints[c + 1].wrapping_add(v6);
        let v11 = ints[c].wrapping_sub(v9);
        let v13 = v10.wrapping_add(v9);
        let v14 = v7.wrapping_add(v11) ^ rotl(v9, 4);
        let v15 = v10.wrapping_sub(v14);
        let v17 = v13.wrapping_add(v14);
        let v18 = v15 ^ rotl(v14, 6);
        let v19 = v13.wrapping_sub(v18);
        let v21 = v17.wrapping_add(v18);
        let v22 = v19 ^ rotl(v18, 8);
        let v23 = v17.wrapping_sub(v22);
        let v25 = v21.wrapping_add(v22);
        let v26 = v23 ^ rotl(v22, 16);
        let v27 = v21.wrapping_sub(v26);
        let v29 = v27 ^ rotr(v26, 13);
        let v30 = v25.wrapping_sub(v29);
        v7 = v25.wrapping_add(v26);
        v6 = v7.wrapping_add(v29);
        v5 = v30 ^ rotl(v29, 4);
        v3 -= 3;
        c += 3;
    }
    if v3 == 3 {
        v5 = v5.wrapping_add(ints[c + 2]);
    }
    if v3 >= 2 {
        v6 = v6.wrapping_add(ints[c + 1]);
    }
    if v3 >= 1 {
        let v34 = (v6 ^ v5).wrapping_sub(rotl(v6, 14));
        let v35 = (v34 ^ v7.wrapping_add(ints[c])).wrapping_sub(rotl(v34, 11));
        let v36 = (v35 ^ v6).wrapping_sub(rotr(v35, 7));
        let v37 = (v36 ^ v34).wrapping_sub(rotl(v36, 16));
        let v38 = rotl(v37, 4);
        let t = (v35 ^ v37).wrapping_sub(v38);
        let v39 = (t ^ v36).wrapping_sub(rotl(t, 14));
        return (v39 ^ v37).wrapping_sub(rotr(v39, 8));
    }
    v5
}

fn tenths(v: f32) -> u32 {
    (v * 10.0) as i64 as u32
}

/// The LOD lights of one placed model. `stored_rot` is the entity's rotation as the file stores it (the inverse of its world rotation);
/// `bb` is the model's box (min, max).
pub fn lights_of_entity(lights: &[DrawableLight], position: [f32; 3], stored_rot: [f32; 4], scale: [f32; 3], bb: ([f32; 3], [f32; 3]), extension_count: usize) -> Vec<LodLight> {
    let q = Quat::from_xyzw(stored_rot[0], stored_rot[1], stored_rot[2], stored_rot[3]);
    let world = if q.length_squared() > 0.0 { q.normalize().conjugate() } else { Quat::IDENTITY };
    let pos = Vec3::from(position);
    let s = Vec3::from(scale);
    // the entity's world box, for the hash
    let (lo, hi) = (Vec3::from(bb.0), Vec3::from(bb.1));
    let mut bmin = Vec3::splat(f32::MAX);
    let mut bmax = Vec3::splat(f32::MIN);
    for i in 0..8 {
        let c = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let w = pos + world * (c * s);
        bmin = bmin.min(w);
        bmax = bmax.max(w);
    }
    let mut ints = [tenths(bmin.x), tenths(bmin.y), tenths(bmin.z), tenths(bmax.x), tenths(bmax.y), tenths(bmax.z), 0];
    lights
        .iter()
        .enumerate()
        .map(|(i, l)| {
            ints[6] = (extension_count + i) as u32;
            let p = pos + world * Vec3::new(l.position.x, l.position.y, l.position.z);
            let d = world * Vec3::new(l.direction.x, l.direction.y, l.direction.z);
            let (r, g, b) = (l.color[0] as u32, l.color[1] as u32, l.color[2] as u32);
            let inten = ((l.intensity * 5.3125).round().clamp(0.0, 255.0)) as u32;
            let kind = l.light_type as u32;
            let mut outer = (l.cone_outer_angle * 1.411_764_7).round().clamp(0.0, 255.0) as u8;
            if kind == 4 {
                outer = (l.extent.x * 1.82).round().clamp(0.0, 255.0) as u8;
            }
            LodLight {
                position: p.to_array(),
                colour: (inten << 24) + (r << 16) + (g << 8) + b,
                direction: d.to_array(),
                falloff: l.falloff,
                falloff_exponent: l.falloff_exponent,
                time_and_state_flags: l.time_flags | (kind << 26),
                hash: compute_light_hash(&ints),
                cone_inner: (l.cone_inner_angle * 1.411_764_7).round().clamp(0.0, 255.0) as u8,
                cone_outer_or_extent: outer,
                corona: if l.corona_size != 0.0 { (l.corona_intensity * 6.0).clamp(0.0, 255.0) as u8 } else { 0 },
                street: false,
            }
        })
        .collect()
}

fn items<T: std::fmt::Display>(v: impl Iterator<Item = T>) -> String {
    v.map(|x| x.to_string()).collect::<Vec<_>>().join(" ")
}

fn vec_items(v: impl Iterator<Item = [f32; 3]>) -> String {
    v.map(|p| format!("<Item x=\"{}\" y=\"{}\" z=\"{}\"/>", p[0], p[1], p[2])).collect()
}

fn doc_from_xml(xml: &str) -> Result<YmapDocument> {
    let mut root = rage_formats::from_xml(xml).context("the generated map")?;
    crate::archetype::schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
    let built = rage_formats::build_meta(&root, rage_formats::Schema::builtin()).context("building the generated map")?;
    YmapDocument::open(&built.bytes, None)
}

/// The two maps for these lights, sorted as the game expects (street lights first, then by hash). Returns (lod lights map, distant lights map).
pub fn build_lod_light_maps(name: &str, mut lights: Vec<LodLight>) -> Result<(YmapDocument, YmapDocument)> {
    lights.sort_by(|a, b| b.street.cmp(&a.street).then(a.hash.cmp(&b.hash)));
    let street = lights.iter().filter(|l| l.street).count();
    let (lod_name, dist_name) = (format!("{name}_lodlights"), format!("{name}_distantlights"));
    let head = |n: &str, parent: &str| format!(r#"<name>{n}</name><parent>{parent}</parent><flags value="0"/><contentFlags value="0"/><streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/><entities/>"#);
    let lod_xml = format!(
        r#"<CMapData>{}<LODLightsSOA><direction itemType="Vector3">{}</direction><falloff itemType="float">{}</falloff><falloffExponent itemType="float">{}</falloffExponent><timeAndStateFlags itemType="uint">{}</timeAndStateFlags><hash itemType="uint">{}</hash><coneInnerAngle itemType="ubyte">{}</coneInnerAngle><coneOuterAngleOrCapExt itemType="ubyte">{}</coneOuterAngleOrCapExt><coronaIntensity itemType="ubyte">{}</coronaIntensity></LODLightsSOA></CMapData>"#,
        head(&lod_name, &dist_name),
        vec_items(lights.iter().map(|l| l.direction)),
        items(lights.iter().map(|l| l.falloff)),
        items(lights.iter().map(|l| l.falloff_exponent)),
        items(lights.iter().map(|l| l.time_and_state_flags)),
        items(lights.iter().map(|l| l.hash)),
        items(lights.iter().map(|l| l.cone_inner)),
        items(lights.iter().map(|l| l.cone_outer_or_extent)),
        items(lights.iter().map(|l| l.corona)),
    );
    let dist_xml = format!(
        r#"<CMapData>{}<DistantLODLightsSOA><position itemType="Vector3">{}</position><RGBI itemType="uint">{}</RGBI><numStreetLights value="{street}"/><category value="1"/></DistantLODLightsSOA></CMapData>"#,
        head(&dist_name, ""),
        vec_items(lights.iter().map(|l| l.position)),
        items(lights.iter().map(|l| l.colour)),
    );
    let mut lod = doc_from_xml(&lod_xml)?;
    let mut dist = doc_from_xml(&dist_xml)?;
    for d in [&mut lod, &mut dist] {
        d.names.add_lowercase(&lod_name);
        d.names.add_lowercase(&dist_name);
        d.update_flags()?;
        d.update_extents(&|_| None, ExtentsMode::Recalculate)?;
    }
    Ok((lod, dist))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rage_formats::math::Vec3 as FVec3;

    fn light(x: f32) -> DrawableLight {
        DrawableLight { position: FVec3 { x, y: 0.0, z: 1.0 }, color: [255, 128, 0], flashiness: 0, intensity: 10.0, flags: 0, bone_id: 0, light_type: 2, group_id: 0, time_flags: 0xFFFFFF, falloff: 5.0, falloff_exponent: 8.0, corona_size: 1.0, corona_intensity: 0.5, direction: FVec3 { x: 1.0, y: 0.0, z: 0.0 }, tangent: FVec3 { x: 0.0, y: 1.0, z: 0.0 }, cone_inner_angle: 10.0, cone_outer_angle: 40.0, extent: FVec3 { x: 1.0, y: 1.0, z: 1.0 } }
    }

    #[test]
    fn the_light_hash_is_stable_and_depends_on_every_input() {
        let a = compute_light_hash(&[1, 2, 3, 4, 5, 6, 0]);
        assert_eq!(a, compute_light_hash(&[1, 2, 3, 4, 5, 6, 0]));
        for k in 0..7 {
            let mut v = [1u32, 2, 3, 4, 5, 6, 0];
            v[k] += 1;
            assert_ne!(a, compute_light_hash(&v), "input {k}");
        }
    }

    #[test]
    fn a_models_lights_are_placed_turned_and_packed_the_usual_way() {
        // a quarter turn about Z (stored inverted) at (100, 0, 0)
        let s = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).conjugate();
        let out = lights_of_entity(&[light(2.0)], [100.0, 0.0, 0.0], [s.x, s.y, s.z, s.w], [1.0; 3], ([-1.0; 3], [1.0; 3]), 0);
        let l = &out[0];
        // the light at (2,0,1) turns to (0,2,1)
        assert!((Vec3::from(l.position) - Vec3::new(100.0, 2.0, 1.0)).length() < 1e-4, "{:?}", l.position);
        assert!((Vec3::from(l.direction) - Vec3::Y).length() < 1e-4);
        assert_eq!(l.colour, (53 << 24) + (255 << 16) + (128 << 8), "intensity 10 * 5.3125 = 53");
        assert_eq!((l.time_and_state_flags >> 26, l.cone_inner, l.cone_outer_or_extent, l.corona), (2, 14, 56, 3));
    }

    #[test]
    fn two_maps_come_out_linked_sorted_and_readable() {
        let lights: Vec<LodLight> = (0..3).flat_map(|i| lights_of_entity(&[light(1.0)], [i as f32 * 10.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0; 3], ([-1.0; 3], [1.0; 3]), 0)).collect();
        let (lod, dist) = build_lod_light_maps("proj", lights).unwrap();
        assert_eq!((lod.name().as_str(), dist.name().as_str()), ("proj_lodlights", "proj_distantlights"));
        assert_eq!(lod.parent_name(), "proj_distantlights");
        assert_eq!((lod.light_count("LODLightsSOA"), dist.light_count("DistantLODLightsSOA")), (3, 3));
        let hashes: Vec<String> = (0..3).map(|i| lod.light_values("LODLightsSOA", i).iter().find(|v| v.0 == "hash").unwrap().1.clone()).collect();
        let nums: Vec<u64> = hashes.iter().map(|h| h.parse().unwrap()).collect();
        assert!(nums.windows(2).all(|w| w[0] <= w[1]), "sorted by hash: {nums:?}");
        let back = YmapDocument::open(&dist.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.light_count("DistantLODLightsSOA"), 3);
    }
}
