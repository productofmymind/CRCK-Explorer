//! What a map's extents and flags should be, worked out from its contents, and checked against the game's own files: the content-flag rules below agree with all 18,948
//! `.ymap` of a retail install for every bit except SLOD (96%).
use crate::ymap_doc::{ArchBounds, YmapDocument};
use anyhow::Result;
use glam::{Quat, Vec3};
use rage_formats::MetaValue;

/// The four boxes a map keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapExtents {
    pub entities_min: [f32; 3],
    pub entities_max: [f32; 3],
    pub streaming_min: [f32; 3],
    pub streaming_max: [f32; 3],
}

/// `contentFlags` bits this module decides; every other bit is left as the file has it.
pub const MANAGED_CONTENT_BITS: u32 = 0x1FF | (1 << 10);

/// One flag, for the properties panel: what it means, whether the file has it, and whether the contents say it should.
#[derive(Debug, Clone)]
pub struct FlagLine {
    /// `"contentFlags"` or `"flags"`
    pub field: &'static str,
    pub bit: u32,
    pub name: &'static str,
    pub meaning: &'static str,
    pub stored: bool,
    /// what the contents say; `None` for a bit this tool does not work out (it is kept as it is)
    pub expected: Option<bool>,
}

impl FlagLine {
    pub fn disagrees(&self) -> bool {
        self.expected.map_or(false, |e| e != self.stored)
    }
}

fn v3(v: &MetaValue) -> Option<Vec3> {
    v.as_vec3().map(|p| Vec3::new(p.x, p.y, p.z))
}

fn field<'a>(v: &'a MetaValue, path: &[&str]) -> Option<&'a MetaValue> {
    let mut cur = v;
    for p in path {
        cur = cur.as_struct()?.field(p)?;
    }
    Some(cur)
}

fn grow(min: &mut Vec3, max: &mut Vec3, lo: Vec3, hi: Vec3) {
    *min = min.min(lo);
    *max = max.max(hi);
}

impl YmapDocument {
    /// The four extents the file stores now (`None` when one of them is missing).
    pub fn stored_extents(&self) -> Option<MapExtents> {
        let get = |n: &str| self.extent(n).map(|p| [p.x, p.y, p.z]);
        Some(MapExtents { entities_min: get("entitiesExtentsMin")?, entities_max: get("entitiesExtentsMax")?, streaming_min: get("streamingExtentsMin")?, streaming_max: get("streamingExtentsMax")? })
    }

    /// The extents to store. Per entity: the archetype's box times the scale, turned by the orientation (the inverse of the
    /// stored rotation) and moved to the position; with no archetype, just the position. The streaming box is that box grown by the entity's
    /// LOD distance (its own, or the archetype's when it is 0 or less). Grass batches, car generators, box occluders, occlude models and
    /// distant LOD lights widen them too. LOD lights are not counted (their positions are not in this file). `None` when nothing
    /// contributes.
    pub fn calc_extents(&self, bounds: &dyn Fn(u32) -> Option<ArchBounds>) -> Option<MapExtents> {
        let big = Vec3::splat(f32::MAX);
        let (mut emin, mut emax, mut smin, mut smax) = (big, -big, big, -big);
        let mut any = false;
        for i in 0..self.len() {
            let Some(e) = self.entity(i) else { continue };
            let pos = Vec3::new(e.position.x, e.position.y, e.position.z);
            let arch = bounds(e.archetype_hash);
            let lod = if e.lod_dist > 0.0 { e.lod_dist } else { arch.map_or(0.0, |a| a.lod_dist.max(0.0)) };
            let l = Vec3::splat(lod);
            let (blo, bhi, sc) = match arch {
                Some(a) => (Vec3::from(a.min), Vec3::from(a.max), Vec3::new(e.scale_xy, e.scale_xy, e.scale_z)),
                None => (Vec3::ZERO, Vec3::ZERO, Vec3::ONE),
            };
            let q = Quat::from_xyzw(e.rotation[0], e.rotation[1], e.rotation[2], e.rotation[3]);
            // map entities store the inverse rotation
            let rot = if q.length_squared() > 0.0 { q.normalize().conjugate() } else { Quat::IDENTITY };
            let (mut elo, mut ehi, mut slo, mut shi) = (big, -big, big, -big);
            for c in 0..8 {
                let corner = |lo: Vec3, hi: Vec3| Vec3::new(if c & 1 == 0 { lo.x } else { hi.x }, if c & 2 == 0 { lo.y } else { hi.y }, if c & 4 == 0 { lo.z } else { hi.z });
                let p = pos + rot * corner(blo * sc, bhi * sc);
                elo = elo.min(p);
                ehi = ehi.max(p);
                let q = pos + rot * corner(blo * sc - l, bhi * sc + l);
                slo = slo.min(q);
                shi = shi.max(q);
            }
            grow(&mut emin, &mut emax, elo, ehi);
            grow(&mut smin, &mut smax, slo, shi);
            any = true;
        }
        // grass batches: their own box, the streaming one grown by the batch's LOD distance
        if let Some(list) = field(&self.root, &["instancedData", "GrassInstanceList"]) {
            for b in list.items() {
                let (Some(lo), Some(hi)) = (field(b, &["BatchAABB", "min"]).and_then(v3), field(b, &["BatchAABB", "max"]).and_then(v3)) else { continue };
                let l = Vec3::splat(field(b, &["lodDist"]).and_then(|v| v.as_f32()).unwrap_or(0.0).max(0.0));
                grow(&mut emin, &mut emax, lo, hi);
                grow(&mut smin, &mut smax, lo - l, hi + l);
                any = true;
            }
        }
        // car generators: the position grown by the length of their parking space (twice that for streaming)
        if let Some(list) = field(&self.root, &["carGenerators"]) {
            for g in list.items() {
                let Some(p) = field(g, &["position"]).and_then(v3) else { continue };
                let len = field(g, &["perpendicularLength"]).and_then(|v| v.as_f32()).unwrap_or(0.0).max(0.0);
                grow(&mut emin, &mut emax, p - Vec3::splat(len), p + Vec3::splat(len));
                grow(&mut smin, &mut smax, p - Vec3::splat(2.0 * len), p + Vec3::splat(2.0 * len));
                any = true;
            }
        }
        // box occluders: stored as quarter-metre integers; a cube round the centre with half the size's length as its radius
        if let Some(list) = field(&self.root, &["boxOccluders"]) {
            for o in list.items() {
                let get = |n: &str| field(o, &[n]).and_then(|v| v.as_f32()).unwrap_or(0.0);
                let centre = Vec3::new(get("iCenterX"), get("iCenterY"), get("iCenterZ")) * 0.25;
                let r = (Vec3::new(get("iLength"), get("iWidth"), get("iHeight")) * 0.25).length() * 0.5;
                grow(&mut emin, &mut emax, centre - Vec3::splat(r), centre + Vec3::splat(r));
                grow(&mut smin, &mut smax, centre - Vec3::splat(r), centre + Vec3::splat(r));
                any = true;
            }
        }
        if let Some(list) = field(&self.root, &["occludeModels"]) {
            for m in list.items() {
                let (Some(lo), Some(hi)) = (field(m, &["bmin"]).and_then(v3), field(m, &["bmax"]).and_then(v3)) else { continue };
                grow(&mut emin, &mut emax, lo, hi);
                grow(&mut smin, &mut smax, lo, hi);
                any = true;
            }
        }
        // distant LOD lights: the box of their positions, grown by 20 m (3000 m for streaming)
        if let Some(list) = field(&self.root, &["DistantLODLightsSOA", "position"]) {
            let pts: Vec<Vec3> = list.items().iter().filter_map(v3).collect();
            if !pts.is_empty() {
                let lo = pts.iter().fold(big, |a, p| a.min(*p));
                let hi = pts.iter().fold(-big, |a, p| a.max(*p));
                grow(&mut emin, &mut emax, lo - Vec3::splat(20.0), hi + Vec3::splat(20.0));
                grow(&mut smin, &mut smax, lo - Vec3::splat(3000.0), hi + Vec3::splat(3000.0));
                any = true;
            }
        }
        any.then(|| MapExtents { entities_min: emin.to_array(), entities_max: emax.to_array(), streaming_min: smin.to_array(), streaming_max: smax.to_array() })
    }

    fn list_len(&self, path: &[&str]) -> usize {
        field(&self.root, path).map_or(0, |v| v.items().len())
    }

    /// (flags bit 1, contentFlags managed bits) from the contents: HD or orphan HD entities set HD; LOD entities set
    /// LOD; SLOD1 set SLOD; SLOD2 to SLOD4 set SLOD2+ and SLOD; an interior instance sets Interior; box occluders or occlude models
    /// set Occlusion; physics dictionaries, LOD lights, distant LOD lights and grass batches set theirs. The "LOD ymap" flag (bit 1 of
    /// `flags`) is set by any LOD or SLOD entity or by distant LOD lights.
    pub fn calc_flags(&self) -> (u32, u32) {
        let (mut flags, mut content) = (0u32, 0u32);
        for i in 0..self.len() {
            let Some(e) = self.entity(i) else { continue };
            if e.is_mlo_instance {
                content |= 1 << 3;
            }
            match e.lod_level {
                0 | 5 => content |= 1,
                1 => {
                    content |= 1 << 1;
                    flags |= 2;
                }
                2 => {
                    content |= 1 << 4;
                    flags |= 2;
                }
                3 | 4 | 6 => {
                    content |= (1 << 2) | (1 << 4);
                    flags |= 2;
                }
                _ => {}
            }
        }
        if self.list_len(&["boxOccluders"]) + self.list_len(&["occludeModels"]) > 0 {
            content |= 1 << 5;
        }
        if self.list_len(&["physicsDictionaries"]) > 0 {
            content |= 1 << 6;
        }
        if self.list_len(&["LODLightsSOA", "direction"]) > 0 {
            content |= 1 << 7;
        }
        if self.list_len(&["DistantLODLightsSOA", "position"]) > 0 {
            content |= 1 << 8;
            flags |= 2;
        }
        if self.list_len(&["instancedData", "GrassInstanceList"]) > 0 {
            content |= 1 << 10;
        }
        (flags, content)
    }

    fn stored_u32(&self, name: &str) -> u32 {
        self.root.as_struct().and_then(|s| s.field(name)).and_then(|v| v.as_u32()).unwrap_or(0)
    }

    /// Every flag with its meaning, the stored value and what the contents say. Bits that this tool does not decide (scripted map, critical,
    /// anything unknown) show `expected: None`.
    pub fn flag_report(&self) -> Vec<FlagLine> {
        let (cf, cc) = self.calc_flags();
        let (sf, sc) = (self.stored_u32("flags"), self.stored_u32("contentFlags"));
        let content: [(u32, &str, &str); 11] = [
            (0, "HD", "Has full-detail entities (and orphan HD ones)"),
            (1, "LOD", "Has LOD stand-in entities"),
            (2, "SLOD2+", "Has SLOD 2, 3 or 4 entities (the far stand-ins)"),
            (3, "Interior", "Places an interior (MLO)"),
            (4, "SLOD", "Has SLOD entities of any level"),
            (5, "Occlusion", "Has occluders (box or model)"),
            (6, "Physics", "Lists physics dictionaries"),
            (7, "LOD lights", "Has LOD lights"),
            (8, "Distant lights", "Has distant LOD lights"),
            (9, "Critical", "Marked critical: must be loaded before the player can move (set by hand, not worked out)"),
            (10, "Grass", "Has grass batches"),
        ];
        let mut out = vec![];
        for (bit, name, meaning) in content {
            let managed = MANAGED_CONTENT_BITS & (1 << bit) != 0;
            out.push(FlagLine { field: "contentFlags", bit, name, meaning, stored: sc & (1 << bit) != 0, expected: managed.then(|| cc & (1 << bit) != 0) });
        }
        out.push(FlagLine { field: "flags", bit: 0, name: "Scripted", meaning: "Only loaded when a script asks for it (set by hand)", stored: sf & 1 != 0, expected: None });
        out.push(FlagLine { field: "flags", bit: 1, name: "LOD map", meaning: "Its entities are LOD stand-ins (has LOD / SLOD entities or distant lights)", stored: sf & 2 != 0, expected: Some(cf & 2 != 0) });
        out
    }

    /// Set the flags the contents decide, keeping every other bit (scripted, critical, unknown ones) as the file has it. One undo step.
    /// True when anything changed.
    pub fn update_flags(&mut self) -> Result<bool> {
        let (cf, cc) = self.calc_flags();
        let (old_f, old_c) = (self.stored_u32("flags"), self.stored_u32("contentFlags"));
        let new_c = (old_c & !MANAGED_CONTENT_BITS) | cc;
        let new_f = (old_f & !2) | cf;
        let mut cmds = vec![];
        if new_c != old_c {
            cmds.push(crate::archetype::edit::Command::Set(vec![crate::archetype::edit::field("contentFlags")], MetaValue::U32(new_c)));
        }
        if new_f != old_f {
            cmds.push(crate::archetype::edit::Command::Set(vec![crate::archetype::edit::field("flags")], MetaValue::U32(new_f)));
        }
        if cmds.is_empty() {
            return Ok(false);
        }
        self.run(crate::archetype::edit::Command::Batch(cmds), "Calculate flags", None)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::schema_view;
    use rage_formats::{build_meta, rage_joaat, Schema};

    fn doc(inner: &str) -> YmapDocument {
        let xml = format!(
            r#"<CMapData><name>t</name><parent/><flags value="0"/><contentFlags value="0"/>
<streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/>
{inner}</CMapData>"#
        );
        let mut root = rage_formats::from_xml(&xml).unwrap();
        schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
        let bytes = build_meta(&root, Schema::builtin()).unwrap().bytes;
        YmapDocument::open(&bytes, None).unwrap()
    }

    fn entity(name: &str, x: f32, y: f32, z: f32, rot: &str, scale: f32, lod: f32, level: &str, mlo: bool) -> String {
        let t = if mlo { "CMloInstanceDef" } else { "CEntityDef" };
        format!(r#"<Item type="{t}"><archetypeName>{name}</archetypeName><flags value="0"/><guid value="1"/><position x="{x}" y="{y}" z="{z}"/><rotation {rot}/><scaleXY value="{scale}"/><scaleZ value="{scale}"/><parentIndex value="-1"/><lodDist value="{lod}"/><childLodDist value="0"/><lodLevel>{level}</lodLevel><numChildren value="0"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>"#)
    }

    const ID: &str = r#"x="0" y="0" z="0" w="1""#;
    const BOX: fn(u32) -> Option<ArchBounds> = |_| Some(ArchBounds { min: [-1.0, -2.0, -3.0], max: [1.0, 2.0, 3.0], lod_dist: 100.0 });

    fn ents(list: &[String]) -> String {
        format!("<entities>{}</entities>", list.join(""))
    }

    #[test]
    fn empty_map_has_no_extents_and_no_flags() {
        let d = doc("<entities/>");
        assert!(d.calc_extents(&BOX).is_none());
        assert_eq!(d.calc_flags(), (0, 0));
    }

    #[test]
    fn one_entity_gives_its_box_and_the_box_grown_by_its_lod_distance() {
        let d = doc(&ents(&[entity("a", 10.0, 20.0, 30.0, ID, 1.0, 50.0, "LODTYPES_DEPTH_HD", false)]));
        let e = d.calc_extents(&BOX).unwrap();
        assert_eq!((e.entities_min, e.entities_max), ([9.0, 18.0, 27.0], [11.0, 22.0, 33.0]));
        assert_eq!((e.streaming_min, e.streaming_max), ([-41.0, -32.0, -23.0], [61.0, 72.0, 83.0]));
    }

    #[test]
    fn a_scaled_entity_scales_the_box_and_an_unset_lod_distance_uses_the_archetypes() {
        let d = doc(&ents(&[entity("a", 0.0, 0.0, 0.0, ID, 2.0, -1.0, "LODTYPES_DEPTH_HD", false)]));
        let e = d.calc_extents(&BOX).unwrap();
        assert_eq!((e.entities_min, e.entities_max), ([-2.0, -4.0, -6.0], [2.0, 4.0, 6.0]));
        // the archetype's 100 is added to the scaled box
        assert_eq!(e.streaming_max, [102.0, 104.0, 106.0]);
    }

    #[test]
    fn a_rotated_entity_swaps_its_axes_and_a_large_one_is_not_just_its_position() {
        // a quarter turn about Z (the file stores the inverse: -45 degrees x/y components swap the long side onto X)
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let d = doc(&ents(&[entity("a", 0.0, 0.0, 0.0, &format!(r#"x="0" y="0" z="{h}" w="{h}""#), 1.0, 0.0, "LODTYPES_DEPTH_HD", false)]));
        let e = d.calc_extents(&|_| Some(ArchBounds { min: [-1.0, -10.0, -1.0], max: [1.0, 10.0, 1.0], lod_dist: 0.0 })).unwrap();
        assert!((e.entities_max[0] - 10.0).abs() < 1e-3 && (e.entities_max[1] - 1.0).abs() < 1e-3, "{:?}", e.entities_max);
        assert!((e.entities_min[0] + 10.0).abs() < 1e-3, "{:?}", e.entities_min);
    }

    #[test]
    fn underground_entities_and_several_lod_distances_are_all_counted() {
        let d = doc(&ents(&[entity("a", 0.0, 0.0, -500.0, ID, 1.0, 10.0, "LODTYPES_DEPTH_HD", false), entity("b", 100.0, 0.0, 5.0, ID, 1.0, 400.0, "LODTYPES_DEPTH_LOD", false)]));
        let e = d.calc_extents(&BOX).unwrap();
        assert_eq!(e.entities_min[2], -503.0);
        assert_eq!(e.streaming_max[0], 101.0 + 400.0);
        assert_eq!(e.streaming_min[2], -503.0 - 10.0);
    }

    #[test]
    fn car_generators_and_distant_lights_widen_the_extents() {
        let d = doc(r#"<entities/><carGenerators itemType="CCarGen"><Item><position x="5" y="5" z="5"/><orientX value="0"/><orientY value="0"/><perpendicularLength value="3"/><carModel>adder</carModel><flags value="0"/><bodyColorRemap1 value="-1"/><bodyColorRemap2 value="-1"/><bodyColorRemap3 value="-1"/><bodyColorRemap4 value="-1"/><popGroup/><livery value="-1"/></Item></carGenerators>"#);
        let e = d.calc_extents(&BOX).unwrap();
        assert_eq!((e.entities_min, e.entities_max), ([2.0; 3], [8.0; 3]));
        assert_eq!((e.streaming_min, e.streaming_max), ([-1.0; 3], [11.0; 3]));
    }

    #[test]
    fn flags_follow_the_contents_and_keep_bits_they_do_not_decide() {
        let list = [entity("a", 0.0, 0.0, 0.0, ID, 1.0, 100.0, "LODTYPES_DEPTH_HD", false), entity("b", 0.0, 0.0, 0.0, ID, 1.0, 100.0, "LODTYPES_DEPTH_SLOD2", false), entity("c", 0.0, 0.0, 0.0, ID, 1.0, 100.0, "LODTYPES_DEPTH_HD", true)];
        let mut d = doc(&ents(&list));
        let (f, c) = d.calc_flags();
        assert_eq!(f, 2, "a SLOD entity makes it a LOD map");
        assert_eq!(c, 1 | (1 << 2) | (1 << 3) | (1 << 4), "HD, SLOD2+, interior, SLOD");
        // the file said critical (bit 9) and scripted (flags bit 0): those stay; a wrong LOD bit goes
        d.run(crate::archetype::edit::Command::Set(vec![crate::archetype::edit::field("contentFlags")], MetaValue::U32((1 << 9) | (1 << 1))), "x", None).unwrap();
        d.run(crate::archetype::edit::Command::Set(vec![crate::archetype::edit::field("flags")], MetaValue::U32(1)), "x", None).unwrap();
        assert!(d.update_flags().unwrap());
        assert_eq!(d.stored_u32("contentFlags"), (1 << 9) | c);
        assert_eq!(d.stored_u32("flags"), 1 | 2);
        assert!(!d.update_flags().unwrap(), "already right");
        let report = d.flag_report();
        assert!(report.iter().all(|l| !l.disagrees()), "{report:?}");
        assert!(report.iter().find(|l| l.name == "Critical").unwrap().expected.is_none());
    }

    #[test]
    fn empty_map_flags_and_a_map_with_only_physics_or_grass() {
        let d = doc("<entities/><physicsDictionaries><Item>some_dict</Item></physicsDictionaries>");
        assert_eq!(d.calc_flags(), (0, 1 << 6));
        let g = doc(r#"<entities/><instancedData><ImapLink/><PropInstanceList itemType="rage__fwPropInstanceListDef"/><GrassInstanceList itemType="rage__fwGrassInstanceListDef"><Item><BatchAABB><min x="0" y="0" z="0" w="0"/><max x="10" y="10" z="10" w="0"/></BatchAABB><ScaleRange x="1" y="1" z="1"/><archetypeName>grass</archetypeName><lodDist value="100"/><LodFadeStartDist value="1"/><LodInstFadeRange value="1"/><OrientToTerrain value="1"/><InstanceList><InstanceList/><Padding/></InstanceList></Item></GrassInstanceList></instancedData>"#);
        assert_eq!(g.calc_flags().1, 1 << 10);
        let e = g.calc_extents(&BOX).unwrap();
        assert_eq!((e.entities_min, e.entities_max, e.streaming_min), ([0.0; 3], [10.0; 3], [-100.0; 3]));
    }
}
