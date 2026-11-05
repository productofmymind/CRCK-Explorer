//! More ways to change a `.ymap` entity list, all through the document's undo history so each is one step: reordering (with the LOD
//! parent indices that follow), changing an entity's archetype, copy and paste, editing any member of an entity or of the map itself by
//! its name, changing several entities' placement at once, and a full check before saving.
use crate::{
    archetype::{
        edit::{field, Command, Step},
        schema_view::{self, FieldKind},
    },
    ymap_calc::MANAGED_CONTENT_BITS,
    ymap_doc::{ArchBounds, MapIssue, Sev, YmapDocument},
};
use anyhow::{bail, Context, Result};
use rage_formats::{rage_joaat, MetaValue, Vec3, Vec4};
use std::collections::HashSet;

const LOD_IN_PARENT_YMAP: u32 = 1 << 3;

/// One member of an entity or of the map, for a properties panel.
#[derive(Debug, Clone)]
pub struct FieldRow {
    pub name: String,
    pub kind: FieldKind,
    /// the value as typed text
    pub text: String,
}

/// A placement: position, the rotation as the file stores it (x, y, z, w), and the two scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub position: Vec3,
    pub rotation: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
}

fn entity_path(i: usize, member: &str) -> Vec<Step> {
    vec![field("entities"), Step::Index(i), field(member)]
}

fn scalar(kind: &FieldKind) -> bool {
    !matches!(kind, FieldKind::Struct(..) | FieldKind::Array(_) | FieldKind::Other)
}

impl YmapDocument {
    /// A guid no entity of this map has yet, derived from `seed`.
    pub fn fresh_guid(&self, seed: u32) -> u32 {
        Self::guid_not_in(&self.used_guids(), seed)
    }

    /// the `guid` of every entity (read straight from the tree: far cheaper than building each entity's view)
    fn used_guids(&self) -> HashSet<u32> {
        let guid = rage_joaat("guid");
        self.entities().iter().filter_map(|v| v.as_struct()?.get(guid)?.as_u32()).collect()
    }

    fn guid_not_in(used: &HashSet<u32>, seed: u32) -> u32 {
        let mut g = seed.wrapping_mul(0x9E37_79B1) ^ 0x5bd1_e995;
        loop {
            g ^= g >> 15;
            g = g.wrapping_mul(0x2C1B_3C6D);
            g ^= g >> 12;
            if g != 0 && !used.contains(&g) {
                return g;
            }
            g = g.wrapping_add(1);
        }
    }

    /// Give entity `i` another archetype (by name). The archetype hash is what the file stores; the name is remembered to show it.
    pub fn set_archetype(&mut self, i: usize, name: &str) -> Result<()> {
        let name = name.trim();
        if i >= self.len() {
            bail!("no entity {i}");
        }
        if name.is_empty() {
            bail!("the entity needs an archetype name");
        }
        self.names.add_lowercase(name);
        let h = schema_view::name_hash(name);
        self.run(Command::Set(entity_path(i, "archetypeName"), MetaValue::Hash(h)), "Change archetype", Some(format!("arch{i}")))
    }

    /// The entities after `i` that have LOD children in other map files (their `numChildren` is more than the children in this file):
    /// removing or moving entity `i` changes their index, and those other files name them by index, so those relations would then point at
    /// the wrong entity.
    pub fn removal_impact(&self, i: usize) -> Vec<usize> {
        let h = self.hierarchy();
        (i + 1..self.len()).filter(|&j| self.entity(j).map_or(false, |e| e.num_children as usize > h.children[j].len())).collect()
    }

    /// Move entity `from` to the place `to` in the list (the others shift to make room). The LOD parent indices of every entity follow, so
    /// the hierarchy is the same afterwards. One undo step. Entities that have LOD children in other map files are reported by
    /// [`YmapDocument::removal_impact`]: those files are not changed.
    pub fn reorder_entity(&mut self, from: usize, to: usize) -> Result<()> {
        let n = self.len();
        if from >= n || to >= n {
            bail!("no such place in the list");
        }
        if from == to {
            return Ok(());
        }
        let map = |old: usize| -> usize {
            if old == from {
                to
            } else if from < to && old > from && old <= to {
                old - 1
            } else if from > to && old >= to && old < from {
                old + 1
            } else {
                old
            }
        };
        let mut cmds = vec![];
        let mut item = self.entities()[from].clone();
        for j in 0..n {
            let Some(e) = self.entity(j) else { continue };
            if e.flags & LOD_IN_PARENT_YMAP != 0 || e.parent_index < 0 || e.parent_index as usize >= n {
                continue;
            }
            let np = map(e.parent_index as usize) as i32;
            if np == e.parent_index {
                continue;
            }
            if j == from {
                // the moved entity is put back from this copy, so its new parent goes into the copy
                if let MetaValue::Struct(s) = &mut item {
                    for (h, v) in s.fields.iter_mut() {
                        if *h == rage_joaat("parentIndex") {
                            *v = MetaValue::I32(np);
                        }
                    }
                }
            } else {
                cmds.push(Command::Set(entity_path(j, "parentIndex"), MetaValue::I32(np)));
            }
        }
        cmds.push(Command::Remove(vec![field("entities")], from));
        cmds.push(Command::Insert(vec![field("entities")], to, item));
        self.run(Command::Batch(cmds), "Reorder entity", None)
    }

    /// Remove several entities as one undo step. Like [`YmapDocument::remove_entity`], it refuses while an entity that stays names one of them
    /// as its LOD parent (or one of them has LOD children in other files); a LOD parent may go together with all its children. The LOD parent
    /// indices of the entities that stay follow the new numbering, and a parent that stays loses the children that went. One pass over the file.
    pub fn remove_entities(&mut self, indices: &[usize]) -> Result<()> {
        let n = self.len();
        let mut gone = vec![false; n];
        for &i in indices {
            if i >= n {
                bail!("no entity {i}: the file has {n}");
            }
            gone[i] = true;
        }
        if !gone.iter().any(|g| *g) {
            return Ok(());
        }
        let views: Vec<Option<crate::ymap_doc::EntityView>> = (0..n).map(|j| self.entity(j)).collect();
        // who names whom (a parent index is only an index into this file when the "parent is in the parent map" flag is off)
        let parent_of = |j: usize| -> Option<usize> {
            let e = views[j].as_ref()?;
            (e.flags & LOD_IN_PARENT_YMAP == 0 && e.parent_index >= 0 && (e.parent_index as usize) < n && e.parent_index as usize != j).then(|| e.parent_index as usize)
        };
        let mut kids = vec![0u32; n];
        let mut staying_kids = vec![0u32; n];
        let mut leaving_kids = vec![0u32; n];
        for j in 0..n {
            if let Some(p) = parent_of(j) {
                kids[p] += 1;
                if gone[j] {
                    leaving_kids[p] += 1;
                } else {
                    staying_kids[p] += 1;
                }
            }
        }
        for p in (0..n).filter(|p| gone[*p]) {
            let external = views[p].as_ref().map_or(0, |e| (e.num_children as usize).saturating_sub(kids[p] as usize));
            let blocked = staying_kids[p] as usize + external;
            if blocked > 0 {
                bail!("entity {p} is the LOD parent of {blocked} other entit{}: remove or unlink them first", if blocked == 1 { "y" } else { "ies" });
            }
        }
        // the new index of every entity that stays
        let mut new_index = vec![0usize; n];
        let mut next = 0;
        for j in 0..n {
            new_index[j] = next;
            if !gone[j] {
                next += 1;
            }
        }
        let mut cmds = vec![];
        for j in (0..n).filter(|j| !gone[*j]) {
            if let Some(p) = parent_of(j) {
                let np = new_index[p] as i32;
                if views[j].as_ref().map_or(false, |e| e.parent_index != np) {
                    cmds.push(Command::Set(entity_path(j, "parentIndex"), MetaValue::I32(np)));
                }
            }
            if leaving_kids[j] > 0 {
                let have = views[j].as_ref().map_or(0, |e| e.num_children);
                cmds.push(Command::Set(entity_path(j, "numChildren"), MetaValue::U32(have.saturating_sub(leaving_kids[j]))));
            }
        }
        for j in (0..n).rev().filter(|j| gone[*j]) {
            cmds.push(Command::Remove(vec![field("entities")], j));
        }
        let count = gone.iter().filter(|g| **g).count();
        self.run(Command::Batch(cmds), if count == 1 { "Remove entity" } else { "Remove entities" }, None)
    }

    /// Copies of the entities at `indices`, to paste into this or another map.
    pub fn copy_entities(&self, indices: &[usize]) -> Vec<MetaValue> {
        indices.iter().filter_map(|&i| self.entities().get(i).cloned()).collect()
    }

    /// Add copies of `items` (from [`YmapDocument::copy_entities`]) moved by `offset`. Each gets a new guid and starts as a root without
    /// children (a copy is not anyone's LOD parent or child). One undo step. Returns the new indices.
    pub fn paste_entities(&mut self, items: &[MetaValue], offset: Vec3) -> Result<Vec<usize>> {
        if items.is_empty() {
            return Ok(vec![]);
        }
        let mut used = self.used_guids();
        let start = self.len();
        let mut cmds = vec![];
        for (k, item) in items.iter().enumerate() {
            let MetaValue::Struct(s) = item else { bail!("that is not an entity") };
            if s.type_hash != rage_joaat("CEntityDef") && s.type_hash != rage_joaat("CMloInstanceDef") {
                bail!("that is not an entity");
            }
            let mut copy = item.clone();
            let guid = Self::guid_not_in(&used, (start + k) as u32 ^ s.get(rage_joaat("guid")).and_then(|v| v.as_u32()).unwrap_or(0));
            used.insert(guid);
            if let MetaValue::Struct(c) = &mut copy {
                for (h, v) in c.fields.iter_mut() {
                    if *h == rage_joaat("guid") {
                        *v = MetaValue::U32(guid);
                    } else if *h == rage_joaat("parentIndex") {
                        *v = MetaValue::I32(-1);
                    } else if *h == rage_joaat("numChildren") {
                        *v = MetaValue::U32(0);
                    } else if *h == rage_joaat("flags") {
                        if let Some(f) = v.as_u32() {
                            *v = MetaValue::U32(f & !LOD_IN_PARENT_YMAP);
                        }
                    } else if *h == rage_joaat("position") {
                        if let Some(p) = v.as_vec3() {
                            *v = MetaValue::Vec3(Vec3::new(p.x + offset.x, p.y + offset.y, p.z + offset.z));
                        }
                    }
                }
            }
            cmds.push(Command::Insert(vec![field("entities")], start + k, copy));
        }
        self.run(Command::Batch(cmds), if items.len() == 1 { "Paste entity" } else { "Paste entities" }, None)?;
        Ok((start..start + items.len()).collect())
    }

    /// A new entity of `archetype` with this placement, added at the end as one undo step (no LOD parent, no children). Returns its index.
    pub fn add_entity_at(&mut self, archetype: &str, p: Placement) -> Result<usize> {
        self.add_entity_as(archetype, p, false)
    }

    /// [`YmapDocument::add_entity_at`]; with `mlo` the entity is a `CMloInstanceDef`, the placement of an interior (its entity sets, group and
    /// portal members start empty and are edited as members).
    pub fn add_entity_as(&mut self, archetype: &str, p: Placement, mlo: bool) -> Result<usize> {
        let archetype = archetype.trim();
        if archetype.is_empty() {
            bail!("the entity needs an archetype name");
        }
        if !(p.scale_xy.is_finite() && p.scale_z.is_finite()) || p.scale_xy <= 0.0 || p.scale_z <= 0.0 {
            bail!("a scale must be above zero");
        }
        let len = p.rotation.iter().map(|c| c * c).sum::<f32>().sqrt();
        if !len.is_finite() || len < 1e-6 {
            bail!("a rotation cannot be zero");
        }
        let at = self.len();
        let mut item = self.new_typed_entity_value(archetype, p.position, at, if mlo { "CMloInstanceDef" } else { "CEntityDef" });
        if let MetaValue::Struct(s) = &mut item {
            for (h, v) in s.fields.iter_mut() {
                if *h == rage_joaat("rotation") {
                    *v = MetaValue::Vec4(Vec4::new(p.rotation[0] / len, p.rotation[1] / len, p.rotation[2] / len, p.rotation[3] / len));
                } else if *h == rage_joaat("scaleXY") {
                    *v = MetaValue::F32(p.scale_xy);
                } else if *h == rage_joaat("scaleZ") {
                    *v = MetaValue::F32(p.scale_z);
                }
            }
        }
        self.names.add_lowercase(archetype);
        self.run(Command::Insert(vec![field("entities")], at, item), "Add entity", None)?;
        Ok(at)
    }

    /// Add many entities at once as one undo step: (archetype, placement, flags, LOD level, LOD distance; a distance of 0 or less keeps the new
    /// entity's default). Returns the new indices. Used by importers.
    pub fn add_entities_bulk(&mut self, list: &[(String, Placement, u32, u8, f32)]) -> Result<Vec<usize>> {
        if list.is_empty() {
            return Ok(vec![]);
        }
        let mut used = self.used_guids();
        let start = self.len();
        let mut cmds = vec![];
        for (k, (name, p, flags, level, lod)) in list.iter().enumerate() {
            let name = name.trim();
            let len = p.rotation.iter().map(|c| c * c).sum::<f32>().sqrt();
            if name.is_empty() || !len.is_finite() || len < 1e-6 || !(p.scale_xy > 0.0 && p.scale_z > 0.0) {
                bail!("entity {k}: it needs a name, a rotation and a scale above zero");
            }
            self.names.add_lowercase(name);
            let mut item = self.new_entity_value(name, p.position, start + k);
            if let MetaValue::Struct(s) = &mut item {
                let guid = Self::guid_not_in(&used, (start + k) as u32 ^ schema_view::name_hash(name));
                used.insert(guid);
                for (h, v) in s.fields.iter_mut() {
                    if *h == rage_joaat("rotation") {
                        *v = MetaValue::Vec4(Vec4::new(p.rotation[0] / len, p.rotation[1] / len, p.rotation[2] / len, p.rotation[3] / len));
                    } else if *h == rage_joaat("scaleXY") {
                        *v = MetaValue::F32(p.scale_xy);
                    } else if *h == rage_joaat("scaleZ") {
                        *v = MetaValue::F32(p.scale_z);
                    } else if *h == rage_joaat("guid") {
                        *v = MetaValue::U32(guid);
                    } else if *h == rage_joaat("flags") {
                        *v = MetaValue::U32(*flags);
                    } else if *h == rage_joaat("lodDist") && *lod > 0.0 {
                        *v = MetaValue::F32(*lod);
                    } else if *h == rage_joaat("lodLevel") {
                        if let MetaValue::Enum { enum_hash, .. } = v {
                            *v = MetaValue::Enum { enum_hash: *enum_hash, value: *level as i32, name: None };
                        }
                    }
                }
            }
            cmds.push(Command::Insert(vec![field("entities")], start + k, item));
        }
        self.run(Command::Batch(cmds), "Add entities", None)?;
        Ok((start..start + list.len()).collect())
    }

    /// The current placement of entity `i`.
    pub fn placement(&self, i: usize) -> Option<Placement> {
        let e = self.entity(i)?;
        Some(Placement { position: e.position, rotation: e.rotation, scale_xy: e.scale_xy, scale_z: e.scale_z })
    }

    /// Set the placement of several entities as one undo step (a group move, rotate or scale). `key` joins consecutive calls with the same
    /// key into the same step (a drag).
    pub fn set_placements(&mut self, list: &[(usize, Placement)], key: Option<String>) -> Result<()> {
        let mut cmds = vec![];
        for (i, p) in list {
            self.need_entity(*i)?;
            if !(p.scale_xy.is_finite() && p.scale_z.is_finite()) || p.scale_xy <= 0.0 || p.scale_z <= 0.0 {
                bail!("a scale must be above zero");
            }
            let len = p.rotation.iter().map(|c| c * c).sum::<f32>().sqrt();
            if !len.is_finite() || len < 1e-6 {
                bail!("a rotation cannot be zero");
            }
            cmds.push(Command::Set(entity_path(*i, "position"), MetaValue::Vec3(p.position)));
            cmds.push(Command::Set(entity_path(*i, "rotation"), MetaValue::Vec4(Vec4::new(p.rotation[0] / len, p.rotation[1] / len, p.rotation[2] / len, p.rotation[3] / len))));
            cmds.push(Command::Set(entity_path(*i, "scaleXY"), MetaValue::F32(p.scale_xy)));
            cmds.push(Command::Set(entity_path(*i, "scaleZ"), MetaValue::F32(p.scale_z)));
        }
        if cmds.is_empty() {
            return Ok(());
        }
        self.run(Command::Batch(cmds), if list.len() == 1 { "Transform entity" } else { "Transform entities" }, key)
    }

    fn need_entity(&self, i: usize) -> Result<()> {
        if i >= self.len() {
            bail!("no entity {i}: the file has {}", self.len());
        }
        Ok(())
    }

    // ---- members by name -------------------------------------------------------------------------------------------------

    fn rows(&self, v: &MetaValue) -> Vec<FieldRow> {
        let Some(s) = v.as_struct() else { return vec![] };
        schema_view::describe(s.type_hash)
            .unwrap_or_default()
            .into_iter()
            .filter(|d| scalar(&d.kind))
            .filter_map(|d| s.get(d.hash).map(|val| FieldRow { name: d.name.clone(), kind: d.kind.clone(), text: schema_view::to_text(val) }))
            .collect()
    }

    /// The simple members of entity `i` (numbers, flags, names, enums, vectors), as text.
    pub fn entity_fields(&self, i: usize) -> Vec<FieldRow> {
        self.entities().get(i).map(|v| self.rows(v)).unwrap_or_default()
    }

    /// Set a member of entity `i` from typed text, by the kind the game's table gives it. A change that would break the LOD hierarchy
    /// (`parentIndex`, `lodLevel`) must go through [`YmapDocument::set_parent`] / [`YmapDocument::set_lod_level`], which check it.
    pub fn set_entity_field(&mut self, i: usize, name: &str, text: &str) -> Result<()> {
        self.need_entity(i)?;
        match name {
            "parentIndex" => bail!("the LOD parent is set with the hierarchy tools"),
            "lodLevel" => {
                let v = schema_view::parse_text(&self.field_kind(i, name)?, text, None)?;
                let MetaValue::Enum { value, .. } = v else { bail!("a LOD level") };
                return self.set_lod_level(i, value.clamp(0, 255) as u8);
            }
            "archetypeName" => return self.set_archetype(i, text),
            _ => {}
        }
        let kind = self.field_kind(i, name)?;
        let old = crate::archetype::edit::get(&self.root, &entity_path(i, name)).cloned();
        let value = schema_view::parse_text(&kind, text, old.as_ref())?;
        self.run(Command::Set(entity_path(i, name), value), &format!("Edit {name}"), Some(format!("field{i}{name}")))
    }

    fn field_kind(&self, i: usize, name: &str) -> Result<FieldKind> {
        let t = self.entities()[i].as_struct().map(|s| s.type_hash).context("that entity is not readable")?;
        schema_view::describe(t).unwrap_or_default().into_iter().find(|d| d.name == name).map(|d| d.kind).with_context(|| format!("an entity has no member called {name}"))
    }

    /// The simple members of the map itself (name, parent, flags, content flags, ...), as text.
    pub fn map_fields(&self) -> Vec<FieldRow> {
        self.rows(&self.root)
    }

    /// Set a simple member of the map from typed text. The managed content-flag bits are better set with
    /// [`YmapDocument::update_flags`]; this lets a modder override one by hand.
    pub fn set_map_field(&mut self, name: &str, text: &str) -> Result<()> {
        let t = self.root.as_struct().map(|s| s.type_hash).context("not a map")?;
        let kind = schema_view::describe(t).unwrap_or_default().into_iter().find(|d| d.name == name).map(|d| d.kind).with_context(|| format!("a map has no member called {name}"))?;
        if !scalar(&kind) {
            bail!("{name} is not edited as text");
        }
        let old = crate::archetype::edit::get(&self.root, &[field(name)]).cloned();
        let value = schema_view::parse_text(&kind, text, old.as_ref())?;
        if matches!(kind, FieldKind::Hash) {
            self.names.add_lowercase(text.trim());
        }
        self.run(Command::Set(vec![field(name)], value), &format!("Edit {name}"), Some(format!("map{name}")))
    }

    /// Turn one bit of `flags` or `contentFlags` on or off by hand (the ones the contents decide can also be set with
    /// [`YmapDocument::update_flags`]). Other bits are kept.
    pub fn set_map_flag(&mut self, field_name: &str, bit: u32, on: bool) -> Result<()> {
        if field_name != "flags" && field_name != "contentFlags" {
            bail!("only flags and contentFlags have bits");
        }
        if bit > 31 {
            bail!("a flag is a bit from 0 to 31");
        }
        let old = self.root.as_struct().and_then(|s| s.field(field_name)).and_then(|v| v.as_u32()).unwrap_or(0);
        let new = if on { old | (1 << bit) } else { old & !(1 << bit) };
        if new == old {
            return Ok(());
        }
        self.run(Command::Set(vec![field(field_name)], MetaValue::U32(new)), &format!("Set {field_name} bit {bit}"), None)
    }

    /// The name of the parent map (the one whose LOD entities these are the children of), or empty.
    pub fn parent_name(&self) -> String {
        let h = self.root.as_struct().and_then(|s| s.field("parent")).and_then(|v| v.as_hash()).unwrap_or(0);
        if h == 0 {
            String::new()
        } else {
            self.names.resolve(h).into_owned()
        }
    }

    // ---- the whole check ---------------------------------------------------------------------------------------------------

    /// Everything wrong or doubtful about the map, before it is saved: the entity checks, the LOD hierarchy, the flags against the
    /// contents, and the extents (invalid, empty while there are entities, or unusually large). Errors first.
    pub fn validate(&self, has_archetype: Option<&dyn Fn(u32) -> bool>, bounds: &dyn Fn(u32) -> Option<ArchBounds>) -> Vec<MapIssue> {
        let mut out: Vec<MapIssue> = self.check(has_archetype);
        // the hierarchy: `check` already covers a parent index that is not an entity and a child count that is too low; add the rest
        for i in self.hierarchy().issues {
            use crate::ymap_lod::LodIssueKind as K;
            if matches!(i.kind, K::ParentNotHigher | K::OrphanParentMismatch | K::ParentIsItself | K::Cycle) {
                out.push(MapIssue { sev: Sev::Error, message: i.message, entity: Some(i.entity) });
            }
        }
        // duplicate guids confuse tools that find entities by guid
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..self.len() {
            if let Some(e) = self.entity(i) {
                if e.guid != 0 && !seen.insert(e.guid) {
                    out.push(MapIssue { sev: Sev::Warning, message: format!("its guid {} is used by another entity of this map", e.guid), entity: Some(i) });
                }
            }
        }
        for line in self.flag_report() {
            if line.disagrees() {
                out.push(MapIssue { sev: Sev::Warning, message: format!("the {} flag is {} but the contents say it should be {} (use Calculate flags)", line.name, if line.stored { "set" } else { "not set" }, if line.stored { "off" } else { "on" }), entity: None });
            }
        }
        let stored = |n: &str| self.root.as_struct().and_then(|s| s.field(n)).and_then(|v| v.as_vec3());
        match (stored("entitiesExtentsMin"), stored("entitiesExtentsMax")) {
            (Some(lo), Some(hi)) => {
                let empty = lo.x == 0.0 && lo.y == 0.0 && lo.z == 0.0 && hi.x == 0.0 && hi.y == 0.0 && hi.z == 0.0;
                if lo.x > hi.x || lo.y > hi.y || lo.z > hi.z || !(lo.x.is_finite() && hi.x.is_finite()) {
                    out.push(MapIssue { sev: Sev::Error, message: "the entity extents are invalid (a minimum above its maximum)".into(), entity: None });
                } else if empty && self.len() > 0 {
                    out.push(MapIssue { sev: Sev::Warning, message: "the entity extents are all zero although the map has entities (use Calculate extents)".into(), entity: None });
                } else if (hi.x - lo.x).max(hi.y - lo.y) > 3000.0 && self.root.as_struct().and_then(|s| s.field("flags")).and_then(|v| v.as_u32()).unwrap_or(0) & 2 == 0 {
                    out.push(MapIssue { sev: Sev::Warning, message: format!("the entity extents are {:.0} m wide, which is unusual for a map that is not a LOD map", (hi.x - lo.x).max(hi.y - lo.y)), entity: None });
                }
            }
            _ => out.push(MapIssue { sev: Sev::Error, message: "the entity extents are missing".into(), entity: None }),
        }
        if let (Some(calc), Some(lo), Some(hi)) = (self.calc_extents(bounds), stored("entitiesExtentsMin"), stored("entitiesExtentsMax")) {
            let outside = calc.entities_min[0] < lo.x - 1.0 || calc.entities_min[1] < lo.y - 1.0 || calc.entities_min[2] < lo.z - 1.0 || calc.entities_max[0] > hi.x + 1.0 || calc.entities_max[1] > hi.y + 1.0 || calc.entities_max[2] > hi.z + 1.0;
            if outside {
                out.push(MapIssue { sev: Sev::Warning, message: "the entities reach past the stored entity extents (use Calculate extents)".into(), entity: None });
            }
        }
        let _ = MANAGED_CONTENT_BITS;
        out.sort_by_key(|i| i.sev != Sev::Error);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::schema_view;
    use rage_formats::{build_meta, Schema};

    /// (archetype, x, parentIndex, numChildren, level)
    fn doc(list: &[(&str, f32, i32, u32, &str)]) -> YmapDocument {
        let items: String = list
            .iter()
            .enumerate()
            .map(|(i, (a, x, p, k, lv))| format!(r#"<Item type="CEntityDef"><archetypeName>{a}</archetypeName><flags value="0"/><guid value="{}"/><position x="{x}" y="0" z="0"/><rotation x="0" y="0" z="0" w="1"/><scaleXY value="1"/><scaleZ value="1"/><parentIndex value="{p}"/><lodDist value="100"/><childLodDist value="0"/><lodLevel>{lv}</lodLevel><numChildren value="{k}"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>"#, i + 1))
            .collect();
        let xml = format!(r#"<CMapData><name>t</name><parent/><flags value="0"/><contentFlags value="0"/><streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/><entities>{items}</entities></CMapData>"#);
        let mut root = rage_formats::from_xml(&xml).unwrap();
        schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
        let mut d = YmapDocument::open(&build_meta(&root, Schema::builtin()).unwrap().bytes, None).unwrap();
        for (a, ..) in list {
            d.names.add_lowercase(a);
        }
        d
    }

    const HD: &str = "LODTYPES_DEPTH_HD";
    const LOD: &str = "LODTYPES_DEPTH_LOD";

    #[test]
    fn entities_are_added_duplicated_and_removed_with_valid_indices() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, -1, 0, HD)]);
        let n = d.add_entity("c", Vec3::new(5.0, 6.0, -100.0)).unwrap();
        assert_eq!((n, d.len(), d.entity_name(2)), (2, 3, "c".into()));
        assert_eq!(d.entity(2).unwrap().position.z, -100.0, "underground is allowed: nothing clamps z");
        let copy = d.duplicate_entity(0).unwrap();
        assert_eq!(d.entity_name(copy), "a");
        let guids: HashSet<u32> = (0..d.len()).map(|i| d.entity(i).unwrap().guid).collect();
        assert_eq!(guids.len(), d.len(), "every guid is different");
        d.remove_entity(1).unwrap();
        assert_eq!(d.len(), 3);
        assert!(d.check(None).is_empty(), "{:?}", d.check(None));
        // everything undoes in order
        for _ in 0..3 {
            d.undo().unwrap();
        }
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn positions_rotations_and_scales_update_and_validate() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD)]);
        d.set_position(0, Vec3::new(1.0, 2.0, -30.0)).unwrap();
        d.set_rotation(0, [0.0, 0.0, 1.0, 1.0]).unwrap();
        d.set_scale(0, 2.0, 3.0).unwrap();
        let e = d.entity(0).unwrap();
        assert_eq!(e.position, Vec3::new(1.0, 2.0, -30.0));
        assert!((e.rotation[2] - 0.7071068).abs() < 1e-5 && (e.rotation[3] - 0.7071068).abs() < 1e-5);
        assert_eq!((e.scale_xy, e.scale_z), (2.0, 3.0));
        assert!(d.set_rotation(0, [0.0; 4]).is_err());
        assert!(d.set_scale(0, 0.0, 1.0).is_err());
        assert!(d.set_scale(0, f32::NAN, 1.0).is_err());
        assert!(d.set_position(5, Vec3::new(0.0, 0.0, 0.0)).is_err());
    }

    #[test]
    fn reordering_keeps_every_parent_relation() {
        // 0: LOD parent of 1 and 2; 3: unrelated HD
        let mut d = doc(&[("p", 0.0, -1, 2, LOD), ("c1", 1.0, 0, 0, HD), ("c2", 2.0, 0, 0, HD), ("o", 3.0, -1, 0, HD)]);
        let before: Vec<(String, Option<String>)> = {
            let h = d.hierarchy();
            (0..4).map(|i| (d.entity_name(i), h.parent[i].map(|p| d.entity_name(p)))).collect()
        };
        d.reorder_entity(0, 3).unwrap();
        assert_eq!(d.entity_name(3), "p");
        let h = d.hierarchy();
        assert!(h.issues.is_empty(), "{:?}", h.issues);
        for i in 0..4 {
            let now = (d.entity_name(i), h.parent[i].map(|p| d.entity_name(p)));
            assert!(before.contains(&now), "{now:?} was not there before");
        }
        // and back
        d.undo().unwrap();
        assert_eq!(d.entity_name(0), "p");
        d.reorder_entity(2, 0).unwrap();
        let h = d.hierarchy();
        assert!(h.issues.is_empty(), "{:?} names {:?} parents {:?}", h.issues, (0..4).map(|i| d.entity_name(i)).collect::<Vec<_>>(), (0..4).map(|i| d.entity(i).unwrap().parent_index).collect::<Vec<_>>());
        assert!(d.reorder_entity(0, 9).is_err());
    }

    #[test]
    fn changing_the_archetype_changes_the_hash_the_file_stores() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD)]);
        d.set_archetype(0, "prop_new_thing").unwrap();
        assert_eq!(d.entity(0).unwrap().archetype_hash, rage_joaat("prop_new_thing"));
        assert_eq!(d.entity_name(0), "prop_new_thing");
        assert!(d.set_archetype(0, "  ").is_err());
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.entity(0).unwrap().archetype_hash, rage_joaat("prop_new_thing"));
    }

    #[test]
    fn copy_and_paste_makes_independent_roots_with_new_guids() {
        let mut d = doc(&[("p", 0.0, -1, 1, LOD), ("c", 1.0, 0, 0, HD)]);
        let clip = d.copy_entities(&[0, 1]);
        let new = d.paste_entities(&clip, Vec3::new(10.0, 0.0, 0.0)).unwrap();
        assert_eq!(new, vec![2, 3]);
        let (a, b) = (d.entity(2).unwrap(), d.entity(3).unwrap());
        assert_eq!((a.parent_index, b.parent_index, a.num_children), (-1, -1, 0), "a copy starts without relations");
        assert_eq!((a.position.x, b.position.x), (10.0, 11.0));
        assert_ne!(a.guid, d.entity(0).unwrap().guid);
        assert!(d.hierarchy().issues.is_empty());
        d.undo().unwrap();
        assert_eq!(d.len(), 2);
        assert!(d.paste_entities(&[MetaValue::U32(1)], Vec3::new(0.0, 0.0, 0.0)).is_err());
    }

    #[test]
    fn members_are_edited_by_name_and_the_hierarchy_ones_are_guarded() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, -1, 0, LOD)]);
        d.set_entity_field(0, "lodDist", "250.5").unwrap();
        d.set_entity_field(0, "tintValue", "7").unwrap();
        assert_eq!(d.entity(0).unwrap().lod_dist, 250.5);
        assert!(d.set_entity_field(0, "tintValue", "banana").is_err());
        assert!(d.set_entity_field(0, "nope", "1").is_err());
        assert!(d.set_entity_field(0, "parentIndex", "1").is_err());
        d.set_entity_field(1, "lodLevel", "LODTYPES_DEPTH_SLOD1").unwrap();
        assert_eq!(d.entity(1).unwrap().lod_level, 2);
        let rows = d.entity_fields(0);
        assert!(rows.iter().any(|r| r.name == "lodDist" && r.text == "250.5"), "{rows:?}");
        d.set_map_field("name", "renamed_map").unwrap();
        assert_eq!(d.name(), "renamed_map");
        d.set_map_field("parent", "some_parent_map").unwrap();
        assert_eq!(d.parent_name(), "some_parent_map");
        assert!(d.set_map_field("entities", "x").is_err());
    }

    #[test]
    fn an_entity_added_with_its_placement_is_one_step_and_flags_toggle_by_bit() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD)]);
        let steps = d.undo_steps();
        let i = d.add_entity_at("prop_here", Placement { position: Vec3::new(1.0, 2.0, -9.0), rotation: [0.0, 0.0, 1.0, 1.0], scale_xy: 2.0, scale_z: 3.0 }).unwrap();
        assert_eq!(d.undo_steps(), steps + 1, "one step, not one for the position, one for the rotation, one for the scale");
        let e = d.entity(i).unwrap();
        assert_eq!((e.position.z, e.scale_xy, e.scale_z), (-9.0, 2.0, 3.0));
        assert!((e.rotation[2] - 0.7071068).abs() < 1e-5);
        assert!(d.add_entity_at("x", Placement { position: Vec3::new(0.0, 0.0, 0.0), rotation: [0.0; 4], scale_xy: 1.0, scale_z: 1.0 }).is_err());
        d.set_map_flag("contentFlags", 9, true).unwrap();
        d.set_map_flag("contentFlags", 0, true).unwrap();
        d.set_map_flag("contentFlags", 9, false).unwrap();
        assert_eq!(d.map_fields().iter().find(|r| r.name == "contentFlags").map(|r| r.text.clone()), Some("1".to_string()));
        assert!(d.set_map_flag("name", 0, true).is_err() && d.set_map_flag("flags", 40, true).is_err());
    }

    #[test]
    fn a_group_move_is_one_undo_step() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, -1, 0, HD), ("c", 2.0, -1, 0, HD)]);
        let list: Vec<(usize, Placement)> = (0..3).map(|i| (i, Placement { position: Vec3::new(i as f32 + 10.0, 0.0, 0.0), rotation: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 })).collect();
        d.set_placements(&list, Some("drag".into())).unwrap();
        let list2: Vec<(usize, Placement)> = (0..3).map(|i| (i, Placement { position: Vec3::new(i as f32 + 20.0, 0.0, 0.0), rotation: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 })).collect();
        d.set_placements(&list2, Some("drag".into())).unwrap();
        assert_eq!((d.entity(0).unwrap().position.x, d.entity(2).unwrap().position.x), (20.0, 22.0));
        d.undo().unwrap();
        assert_eq!((d.entity(0).unwrap().position.x, d.entity(2).unwrap().position.x), (0.0, 2.0), "the whole drag undid in one step");
        assert!(d.set_placements(&[(9, list[0].1)], None).is_err());
    }

    #[test]
    fn several_entities_are_removed_in_one_step_and_the_parent_indices_follow() {
        // 1 is the LOD parent of 3 and 4
        let mut d = doc(&[("r0", 0.0, -1, 0, HD), ("p1", 1.0, -1, 2, LOD), ("r2", 2.0, -1, 0, HD), ("c3", 3.0, 1, 0, HD), ("c4", 4.0, 1, 0, HD), ("r5", 5.0, -1, 0, HD)]);
        let steps = d.undo_steps();
        d.remove_entities(&[4, 0, 2, 2]).unwrap();
        assert_eq!(d.undo_steps(), steps + 1, "one undo step for all of them, a repeated index counts once");
        let names: Vec<String> = (0..d.len()).map(|i| d.entity_name(i)).collect();
        assert_eq!(names, vec!["p1", "c3", "r5"]);
        assert_eq!((d.entity(1).unwrap().parent_index, d.entity(0).unwrap().num_children), (0, 1), "c3 now names p1 at its new place, and p1 lost c4");
        assert!(d.hierarchy().issues.is_empty());
        d.undo().unwrap();
        assert_eq!(d.len(), 6);
        assert_eq!((d.entity(3).unwrap().parent_index, d.entity(4).unwrap().parent_index, d.entity(1).unwrap().num_children), (1, 1, 2));
        d.redo().unwrap();
        assert_eq!(d.len(), 3);
        // the saved file reads back the same
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.entity(1).unwrap().parent_index, 0);
    }

    #[test]
    fn a_whole_lod_group_can_go_together_but_a_parent_cannot_leave_its_children() {
        let mut d = doc(&[("p", 0.0, -1, 2, LOD), ("c1", 1.0, 0, 0, HD), ("c2", 2.0, 0, 0, HD), ("o", 3.0, -1, 0, HD)]);
        let e = d.remove_entities(&[0]).unwrap_err().to_string();
        assert!(e.contains("entity 0") && e.contains("2 other entities"), "{e}");
        assert!(d.remove_entities(&[0, 1]).is_err(), "c2 would be left without its parent");
        assert_eq!(d.len(), 4, "a refused removal changes nothing");
        d.remove_entities(&[0, 1, 2]).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d.entity_name(0), "o");
        // children in other files (numChildren above the children seen here) block too
        let mut d = doc(&[("p", 0.0, -1, 5, LOD), ("c", 1.0, 0, 0, HD)]);
        assert!(d.remove_entities(&[0, 1]).is_err(), "p has children in other maps");
        assert!(d.remove_entities(&[7]).is_err());
        d.remove_entities(&[]).unwrap();
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn removal_leaves_a_parent_map_index_alone_and_removes_a_damaged_entity() {
        // entity 1 points into the parent map (flag 8), index 3: removing entity 0 must not renumber it
        let mut d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, 3, 0, HD)]);
        d.set_entity_field(1, "flags", "8").unwrap();
        d.remove_entities(&[0]).unwrap();
        assert_eq!(d.entity(0).unwrap().parent_index, 3);
    }

    #[test]
    fn many_entities_are_added_in_one_step_with_their_flags_levels_and_distances() {
        let mut d = doc(&[]);
        let p = |x: f32| Placement { position: Vec3::new(x, 0.0, 0.0), rotation: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 };
        let steps = d.undo_steps();
        let idx = d.add_entities_bulk(&[("a".into(), p(1.0), 32, 5, 500.0), ("b".into(), p(2.0), 0, 0, 0.0)]).unwrap();
        assert_eq!((idx, d.undo_steps()), (vec![0, 1], steps + 1));
        let (a, b) = (d.entity(0).unwrap(), d.entity(1).unwrap());
        assert_eq!((a.flags, a.lod_level, a.lod_dist, a.position.x), (32, 5, 500.0, 1.0));
        assert_eq!((b.lod_level, b.lod_dist), (0, 100.0), "no distance given keeps the default");
        assert_ne!(a.guid, b.guid);
        assert!(d.add_entities_bulk(&[("".into(), p(0.0), 0, 0, 0.0)]).is_err());
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.entity(0).unwrap().lod_level, 5);
    }

    #[test]
    fn an_interior_is_placed_as_an_mlo_instance_and_saves() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD)]);
        let p = Placement { position: Vec3::new(1.0, 2.0, 3.0), rotation: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 };
        let i = d.add_entity_as("v_int_test", p, true).unwrap();
        assert!(d.entity(i).unwrap().is_mlo_instance);
        assert!(!d.entity(0).unwrap().is_mlo_instance);
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert!(back.entity(1).unwrap().is_mlo_instance);
        assert_eq!(back.entity(1).unwrap().position, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn removal_impact_names_the_entities_whose_index_other_files_use() {
        // entity 2 says it has 3 children but only one is in this file: the rest are in child maps that name it by index
        let d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, -1, 0, HD), ("p", 2.0, -1, 3, LOD), ("c", 3.0, 2, 0, HD)]);
        assert_eq!(d.removal_impact(0), vec![2]);
        assert_eq!(d.removal_impact(2), Vec::<usize>::new());
    }

    #[test]
    fn validation_collects_errors_first_and_checks_extents_and_flags() {
        let mut d = doc(&[("a", 0.0, -1, 0, HD), ("b", 1.0, 5, 0, HD)]);
        let bounds = |_: u32| Some(ArchBounds { min: [-1.0; 3], max: [1.0; 3], lod_dist: 100.0 });
        let v = d.validate(None, &bounds);
        assert!(v[0].sev == Sev::Error, "{v:?}");
        assert!(v.iter().any(|i| i.message.contains("extents are all zero")), "{v:?}");
        assert!(v.iter().any(|i| i.message.contains("HD flag")), "{v:?}");
        // fix: parent index back, extents and flags calculated
        d.set_parent(1, None).unwrap_or(());
        d.run(Command::Set(entity_path(1, "parentIndex"), MetaValue::I32(-1)), "x", None).unwrap();
        d.update_extents(&bounds, crate::ymap_doc::ExtentsMode::Recalculate).unwrap();
        d.update_flags().unwrap();
        assert!(d.validate(None, &bounds).is_empty(), "{:?}", d.validate(None, &bounds));
    }
}
