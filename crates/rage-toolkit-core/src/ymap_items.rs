//! The other lists of a `.ymap`, edited as generic structures driven by the game's table: car generators, box occluders, occlude models,
//! time cycle modifiers, and the extensions (particle effects, doors, ladders, lights, spawn points, ...) of an entity. Every member the
//! table lists is editable as text; members it does not list are never touched. All edits are single undo steps.
use crate::{
    archetype::{
        edit::{field, get, Command, Step},
        schema_view,
    },
    ymap_doc::YmapDocument,
    ymap_ops::FieldRow,
};
use anyhow::{bail, Context, Result};
use rage_formats::{rage_joaat, MetaValue, Vec3};

/// a list of items: one of the map's own, or the extensions of an entity
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemList {
    Map(&'static str),
    Extensions(usize),
}

/// (title, member of `CMapData`, structure of its items)
pub const MAP_LISTS: [(&str, &str, &str); 5] = [
    ("Car generators", "carGenerators", "CCarGen"),
    ("Box occluders", "boxOccluders", "BoxOccluder"),
    ("Occlude models", "occludeModels", "OccludeModel"),
    ("Time cycle modifiers", "timeCycleModifiers", "CTimeCycleModifier"),
    ("Grass batches", "instancedData/GrassInstanceList", "rage__fwGrassInstanceListDef"),
];

/// A value of the same kind as `old` from typed numbers (for list members the table does not describe: vectors and plain numbers).
fn parse_like(old: &MetaValue, text: &str) -> Result<MetaValue> {
    let nums: Vec<f64> = text.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.parse::<f64>().map_err(|_| anyhow::anyhow!("{t} is not a number"))).collect::<Result<_>>()?;
    let need = |n: usize| if nums.len() == n { Ok(()) } else { Err(anyhow::anyhow!("{n} number{} needed", if n == 1 { "" } else { "s" })) };
    Ok(match old {
        MetaValue::Vec3(_) => {
            need(3)?;
            MetaValue::Vec3(Vec3::new(nums[0] as f32, nums[1] as f32, nums[2] as f32))
        }
        MetaValue::F32(_) => {
            need(1)?;
            MetaValue::F32(nums[0] as f32)
        }
        MetaValue::U32(_) => {
            need(1)?;
            MetaValue::U32(nums[0] as u32)
        }
        MetaValue::I32(_) => {
            need(1)?;
            MetaValue::I32(nums[0] as i32)
        }
        _ => bail!("this kind of value is not edited as text"),
    })
}

fn list_path(l: ItemList) -> Vec<Step> {
    match l {
        ItemList::Map(n) => n.split('/').map(field).collect(),
        ItemList::Extensions(i) => vec![field("entities"), Step::Index(i), field("extensions")],
    }
}

fn item_path(l: ItemList, i: usize, member: &str) -> Vec<Step> {
    let mut p = list_path(l);
    p.push(Step::Index(i));
    p.push(field(member));
    p
}

impl YmapDocument {
    fn list_items(&self, l: ItemList) -> &[MetaValue] {
        get(&self.root, &list_path(l)).map(|v| v.items()).unwrap_or(&[])
    }

    pub fn item_count(&self, l: ItemList) -> usize {
        self.list_items(l).len()
    }

    /// the structure name of item `i` (`CCarGen`, `CExtensionDefDoor`, ...)
    pub fn item_type(&self, l: ItemList, i: usize) -> String {
        self.list_items(l).get(i).and_then(|v| v.as_struct()).map_or(String::new(), |s| schema_view::name_of(s.type_hash))
    }

    /// the simple members of item `i` as text
    pub fn item_fields(&self, l: ItemList, i: usize) -> Vec<FieldRow> {
        let Some(s) = self.list_items(l).get(i).and_then(|v| v.as_struct()) else { return vec![] };
        schema_view::describe(s.type_hash)
            .unwrap_or_default()
            .into_iter()
            .filter(|d| !matches!(d.kind, schema_view::FieldKind::Struct(..) | schema_view::FieldKind::Array(_) | schema_view::FieldKind::Other))
            .filter_map(|d| s.get(d.hash).map(|v| FieldRow { name: d.name.clone(), kind: d.kind.clone(), text: schema_view::to_text(v) }))
            .collect()
    }

    /// the structures an item of this list may be: the list's own, or any extension
    pub fn item_types(l: ItemList) -> Vec<(String, u32)> {
        match l {
            ItemList::Map(n) => MAP_LISTS.iter().filter(|m| m.1 == n).map(|m| (m.2.to_string(), rage_joaat(m.2))).collect(),
            ItemList::Extensions(_) => schema_view::struct_names_with_prefix("CExtensionDef").into_iter().filter(|(n, _)| n != "CExtensionDef").collect(),
        }
    }

    /// Add an item of structure `type_hash` (every member of the table empty), placed at `at` when it has a position. Returns its index.
    pub fn add_item(&mut self, l: ItemList, type_hash: u32, at: Option<Vec3>) -> Result<usize> {
        if let ItemList::Extensions(i) = l {
            if i >= self.len() {
                bail!("no entity {i}");
            }
        }
        if !Self::item_types(l).iter().any(|t| t.1 == type_hash) {
            bail!("that kind of item does not belong in this list");
        }
        let mut item = schema_view::new_struct(type_hash);
        if let (MetaValue::Struct(s), Some(p)) = (&mut item, at) {
            for (h, v) in s.fields.iter_mut() {
                if *h == rage_joaat("position") || *h == rage_joaat("offsetPosition") {
                    *v = MetaValue::Vec3(p);
                } else if *h == rage_joaat("iCenterX") {
                    *v = MetaValue::I32((p.x * 4.0).round() as i32);
                } else if *h == rage_joaat("iCenterY") {
                    *v = MetaValue::I32((p.y * 4.0).round() as i32);
                } else if *h == rage_joaat("iCenterZ") {
                    *v = MetaValue::I32((p.z * 4.0).round() as i32);
                }
            }
        }
        let at_index = self.item_count(l);
        self.run(Command::Insert(list_path(l), at_index, item), "Add item", None)?;
        Ok(at_index)
    }

    pub fn remove_item(&mut self, l: ItemList, i: usize) -> Result<()> {
        if i >= self.item_count(l) {
            bail!("no item {i}");
        }
        self.run(Command::Remove(list_path(l), i), "Remove item", None)
    }

    pub fn duplicate_item(&mut self, l: ItemList, i: usize) -> Result<usize> {
        let copy = self.list_items(l).get(i).cloned().with_context(|| format!("no item {i}"))?;
        let at = self.item_count(l);
        self.run(Command::Insert(list_path(l), at, copy), "Duplicate item", None)?;
        Ok(at)
    }

    /// Set one member of an item from typed text, by the kind the table gives it.
    pub fn set_item_field(&mut self, l: ItemList, i: usize, name: &str, text: &str) -> Result<()> {
        let s = self.list_items(l).get(i).and_then(|v| v.as_struct()).with_context(|| format!("no item {i}"))?;
        let kind = schema_view::describe(s.type_hash).unwrap_or_default().into_iter().find(|d| d.name == name).map(|d| d.kind).with_context(|| format!("this item has no member called {name}"))?;
        let old = get(&self.root, &item_path(l, i, name)).cloned();
        let value = schema_view::parse_text(&kind, text, old.as_ref())?;
        self.run(Command::Set(item_path(l, i, name), value), &format!("Edit {name}"), Some(format!("item{i}{name}")))
    }

    // ---- LOD lights and distant lights: parallel arrays, one entry per light in each ------------------------------------------------

    fn light_arrays(&self, group: &str) -> Vec<String> {
        let Some(s) = self.root.as_struct().and_then(|r| r.get(rage_joaat(group))).and_then(|v| v.as_struct()) else { return vec![] };
        schema_view::describe(s.type_hash).unwrap_or_default().into_iter().filter(|d| matches!(d.kind, schema_view::FieldKind::Array(_))).map(|d| d.name).collect()
    }

    fn light_array(&self, group: &str, array: &str) -> &[MetaValue] {
        get(&self.root, &[field(group), field(array)]).map(|v| v.items()).unwrap_or(&[])
    }

    /// how many lights `group` (`LODLightsSOA` or `DistantLODLightsSOA`) holds
    pub fn light_count(&self, group: &str) -> usize {
        self.light_arrays(group).iter().map(|a| self.light_array(group, a).len()).max().unwrap_or(0)
    }

    /// light `i`: each of the group's arrays with this light's value as text
    pub fn light_values(&self, group: &str, i: usize) -> Vec<(String, String)> {
        self.light_arrays(group).into_iter().filter_map(|a| self.light_array(group, &a).get(i).map(|v| (a.clone(), schema_view::to_text(v)))).collect()
    }

    pub fn set_light_value(&mut self, group: &str, i: usize, array: &str, text: &str) -> Result<()> {
        let old = self.light_array(group, array).get(i).cloned().with_context(|| format!("no light {i}"))?;
        let t = self.root.as_struct().and_then(|r| r.get(rage_joaat(group))).and_then(|v| v.as_struct()).map(|s| s.type_hash).context("no such group")?;
        let kind = match schema_view::describe(t).unwrap_or_default().into_iter().find(|d| d.name == array).map(|d| d.kind) {
            Some(schema_view::FieldKind::Array(inner)) => *inner,
            _ => bail!("{array} is not a list of the group"),
        };
        let value = match kind {
            schema_view::FieldKind::Other => parse_like(&old, text)?,
            k => schema_view::parse_text(&k, text, Some(&old)).or_else(|_| parse_like(&old, text))?,
        };
        self.run(Command::Set(vec![field(group), field(array), Step::Index(i)], value), "Edit light", Some(format!("light{group}{i}{array}")))
    }

    /// Take light `i` out of every array of the group (one step).
    pub fn remove_light(&mut self, group: &str, i: usize) -> Result<()> {
        if i >= self.light_count(group) {
            bail!("no light {i}");
        }
        let cmds: Vec<Command> = self.light_arrays(group).into_iter().filter(|a| self.light_array(group, a).len() > i).map(|a| Command::Remove(vec![field(group), field(&a)], i)).collect();
        self.run(Command::Batch(cmds), "Remove light", None)
    }

    /// A copy of light `i` at the end of every array (one step). Returns its index.
    pub fn duplicate_light(&mut self, group: &str, i: usize) -> Result<usize> {
        let n = self.light_count(group);
        if i >= n {
            bail!("no light {i}");
        }
        let cmds: Vec<Command> = self.light_arrays(group).into_iter().filter_map(|a| self.light_array(group, &a).get(i).cloned().map(|v| Command::Insert(vec![field(group), field(&a)], self.light_array(group, &a).len(), v))).collect();
        self.run(Command::Batch(cmds), "Duplicate light", None)?;
        Ok(n)
    }

    /// Delete the grass instances within `radius` metres of `centre` (all batches), as one undo step; returns how many went. An instance's
    /// position is three 16-bit numbers spread over its batch's box (`BatchAABB`).
    pub fn delete_grass_in_sphere(&mut self, centre: Vec3, radius: f32) -> Result<usize> {
        let list = ItemList::Map("instancedData/GrassInstanceList");
        let mut cmds = vec![];
        let mut count = 0;
        for b in 0..self.item_count(list) {
            let Some(batch) = self.list_items(list).get(b).and_then(|v| v.as_struct()) else { continue };
            let Some(aabb) = batch.get(rage_joaat("BatchAABB")).and_then(|v| v.as_struct()) else { continue };
            let (Some(lo), Some(hi)) = (aabb.get(rage_joaat("min")).and_then(|v| v.as_vec3()), aabb.get(rage_joaat("max")).and_then(|v| v.as_vec3())) else { bail!("grass batch {b} has no box") };
            let Some(inst) = batch.get(rage_joaat("InstanceList")) else { continue };
            let mut path = list_path(list);
            path.push(Step::Index(b));
            path.push(field("InstanceList"));
            let mut doomed = vec![];
            for (i, item) in inst.items().iter().enumerate() {
                let Some(p) = item.as_struct().and_then(|s| s.get(rage_joaat("Position"))) else { continue };
                let c: Vec<f32> = p.items().iter().filter_map(|v| v.as_u32()).map(|v| v as f32 / 65535.0).collect();
                if c.len() < 3 {
                    bail!("grass batch {b}: an instance position is not three numbers");
                }
                let world = Vec3::new(lo.x + c[0] * (hi.x - lo.x), lo.y + c[1] * (hi.y - lo.y), lo.z + c[2] * (hi.z - lo.z));
                let d = (world.x - centre.x).powi(2) + (world.y - centre.y).powi(2) + (world.z - centre.z).powi(2);
                if d <= radius * radius {
                    doomed.push(i);
                }
            }
            count += doomed.len();
            for i in doomed.into_iter().rev() {
                cmds.push(Command::Remove(path.clone(), i));
            }
        }
        if cmds.is_empty() {
            return Ok(0);
        }
        self.run(Command::Batch(cmds), "Delete grass", None)?;
        Ok(count)
    }

    /// where item `i` is, for the ones that have a position (car generators, occluders, extensions with an offset)
    pub fn item_position(&self, l: ItemList, i: usize) -> Option<Vec3> {
        let s = self.list_items(l).get(i)?.as_struct()?;
        for n in ["position", "offsetPosition"] {
            if let Some(p) = s.get(rage_joaat(n)).and_then(|v| v.as_vec3()) {
                return Some(p);
            }
        }
        let c = |n: &str| s.get(rage_joaat(n)).and_then(|v| v.as_i64());
        Some(Vec3::new(c("iCenterX")? as f32 / 4.0, c("iCenterY")? as f32 / 4.0, c("iCenterZ")? as f32 / 4.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn car_generators_and_occluders_are_added_edited_duplicated_removed_and_saved() {
        let mut d = YmapDocument::new_map("items").unwrap();
        let cg = ItemList::Map("carGenerators");
        let (ty, hash) = YmapDocument::item_types(cg)[0].clone();
        assert_eq!(ty, "CCarGen");
        let steps = d.undo_steps();
        let i = d.add_item(cg, hash, Some(Vec3::new(1.0, 2.0, -30.0))).unwrap();
        assert_eq!((i, d.undo_steps()), (0, steps + 1));
        assert_eq!(d.item_position(cg, 0), Some(Vec3::new(1.0, 2.0, -30.0)));
        assert_eq!(d.item_type(cg, 0), "CCarGen");
        let rows = d.item_fields(cg, 0);
        assert!(rows.iter().any(|r| r.name == "position"), "{rows:?}");
        d.set_item_field(cg, 0, "position", "5 6 7").unwrap();
        assert_eq!(d.item_position(cg, 0), Some(Vec3::new(5.0, 6.0, 7.0)));
        assert!(d.set_item_field(cg, 0, "nope", "1").is_err());
        d.duplicate_item(cg, 0).unwrap();
        assert_eq!(d.item_count(cg), 2);
        let oc = ItemList::Map("boxOccluders");
        let (_, oh) = YmapDocument::item_types(oc)[0].clone();
        d.add_item(oc, oh, Some(Vec3::new(10.0, 20.0, 30.0))).unwrap();
        assert_eq!(d.item_position(oc, 0), Some(Vec3::new(10.0, 20.0, 30.0)), "occluders store quarter metres");
        assert!(d.add_item(oc, hash, None).is_err(), "a car generator is not an occluder");
        d.update_flags().unwrap();
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!((back.item_count(cg), back.item_count(oc)), (2, 1));
        d.remove_item(cg, 1).unwrap();
        d.undo().unwrap();
        assert_eq!(d.item_count(cg), 2);
        assert!(d.remove_item(cg, 9).is_err());
    }

    #[test]
    fn extensions_of_an_entity_are_a_list_of_the_games_extension_structures() {
        let mut d = YmapDocument::new_map("ext").unwrap();
        d.add_entity("prop_a", Vec3::new(0.0, 0.0, 0.0)).unwrap();
        let l = ItemList::Extensions(0);
        let kinds = YmapDocument::item_types(l);
        assert!(!kinds.is_empty(), "the table lists extension structures");
        let (_, h) = kinds[0].clone();
        d.add_item(l, h, Some(Vec3::new(1.0, 1.0, 1.0))).unwrap();
        assert_eq!(d.item_count(l), 1);
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.item_count(l), 1);
        assert!(d.add_item(ItemList::Extensions(5), h, None).is_err());
    }
}

#[cfg(test)]
mod grass_tests {
    use super::*;

    #[test]
    fn grass_batches_live_under_instanced_data_and_edit_like_any_item() {
        let mut d = YmapDocument::new_map("grass").unwrap();
        let l = ItemList::Map("instancedData/GrassInstanceList");
        let kinds = YmapDocument::item_types(l);
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        d.add_item(l, kinds[0].1, None).unwrap();
        assert_eq!(d.item_count(l), 1);
        let rows = d.item_fields(l, 0);
        assert!(rows.iter().any(|r| r.name == "lodDist"), "{rows:?}");
        d.set_item_field(l, 0, "lodDist", "120").unwrap();
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.item_count(l), 1);
    }
}

#[cfg(test)]
mod delete_grass_tests {
    use super::*;
    use crate::archetype::schema_view as sv;
    use rage_formats::{build_meta, Schema};

    fn doc_with_grass() -> YmapDocument {
        // a batch over (0,0,0)-(100,100,10) with instances at the corners and the middle
        let inst = |x: u32, y: u32, z: u32| format!(r#"<Item><Position itemType="ushort">{x} {y} {z}</Position><NormalX value="0"/><NormalY value="0"/><Color itemType="ByteArray">00 00 00</Color><Scale value="1"/><Ao value="255"/><Pad itemType="ByteArray">00 00 00</Pad></Item>"#);
        let items = [inst(0, 0, 0), inst(65535, 65535, 65535), inst(32768, 32768, 32768), inst(1000, 1000, 0)].concat();
        let xml = format!(r#"<CMapData><name>g</name><parent/><flags value="0"/><contentFlags value="0"/><streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/><entities/><instancedData><ImapLink/><PropInstanceList itemType="rage__fwPropInstanceListDef"/><GrassInstanceList itemType="rage__fwGrassInstanceListDef"><Item><BatchAABB><min x="0" y="0" z="0" w="0"/><max x="100" y="100" z="10" w="0"/></BatchAABB><ScaleRange x="1" y="1" z="1"/><archetypeName>proc_grass01</archetypeName><lodDist value="50"/><LodFadeStartDist value="10"/><LodInstFadeRange value="1"/><OrientToTerrain value="1"/><InstanceList itemType="rage__fwGrassInstanceListDef__InstanceData">{items}</InstanceList></Item></GrassInstanceList></instancedData></CMapData>"#);
        let mut root = rage_formats::from_xml(&xml).unwrap();
        sv::coerce_tree(&mut root, rage_joaat("CMapData"));
        YmapDocument::open(&build_meta(&root, Schema::builtin()).unwrap().bytes, None).unwrap()
    }

    #[test]
    fn grass_inside_a_sphere_is_deleted_in_one_step_and_the_rest_stays() {
        let mut d = doc_with_grass();
        let steps = d.undo_steps();
        // the corner (0,0,0) and the near one (1.5,1.5,0) are within 5 m of the origin; the middle and the far corner are not
        let n = d.delete_grass_in_sphere(Vec3::new(0.0, 0.0, 0.0), 5.0).unwrap();
        assert_eq!((n, d.undo_steps()), (2, steps + 1));
        assert_eq!(d.delete_grass_in_sphere(Vec3::new(0.0, 0.0, 0.0), 5.0).unwrap(), 0, "nothing left there, no step");
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        let xml = back.to_xml();
        assert_eq!(xml.matches("<NormalX").count(), 2, "{xml}");
        d.undo().unwrap();
        assert_eq!(d.to_xml().matches("<NormalX").count(), 4);
    }
}

#[cfg(test)]
mod light_tests {
    use super::*;
    use crate::archetype::schema_view as sv;
    use rage_formats::{build_meta, Schema};

    #[test]
    fn distant_lights_are_edited_duplicated_and_removed_across_their_arrays() {
        let xml = r#"<CMapData><name>l</name><parent/><flags value="0"/><contentFlags value="0"/><streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/><entities/><DistantLODLightsSOA><position itemType="Vector3"><Item x="1" y="2" z="3"/><Item x="4" y="5" z="6"/></position><RGBI itemType="uint">4278190335 16711935</RGBI><numStreetLights value="2"/><category value="0"/></DistantLODLightsSOA></CMapData>"#;
        let mut root = rage_formats::from_xml(xml).unwrap();
        sv::coerce_tree(&mut root, rage_joaat("CMapData"));
        let mut d = YmapDocument::open(&build_meta(&root, Schema::builtin()).unwrap().bytes, None).unwrap();
        let g = "DistantLODLightsSOA";
        assert_eq!(d.light_count(g), 2);
        assert_eq!(d.light_values(g, 1)[0].0, "position");
        d.set_light_value(g, 1, "position", "40 50 60").unwrap();
        assert!(d.light_values(g, 1)[0].1.contains("40"), "{:?}", d.light_values(g, 1));
        assert_eq!(d.duplicate_light(g, 0).unwrap(), 2);
        assert_eq!(d.light_count(g), 3);
        d.remove_light(g, 0).unwrap();
        assert_eq!(d.light_count(g), 2);
        assert!(d.remove_light(g, 9).is_err());
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.light_count(g), 2);
        assert_eq!(back.light_values(g, 1).len(), d.light_values(g, 1).len());
    }
}
