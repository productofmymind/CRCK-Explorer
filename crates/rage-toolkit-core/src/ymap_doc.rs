//! An open `.ymap`: the whole file as a `MetaValue` tree (so car generators, occluders, LOD lights, grass and everything else this
//! module does not edit are kept as they are), entity edits with undo and redo, checks, and safe saving. The same design as the
//! `.ytyp` editor (`archetype::document`), which it reuses: [`History`] for undo, [`compare_trees`] for the read-back check.
//!
//! Only the entity list is edited here. `entitiesExtents` and `streamingExtents` are **not** recomputed (that needs every
//! archetype's bounding box); [`YmapDocument::check`] warns when an entity lies outside them.
use crate::archetype::{
    edit::{field, Command, History, Path, Step},
    fidelity::{compare_trees, Container},
    schema_view,
};
use anyhow::{bail, Context, Result};
/// for callers that do not depend on `rage-formats` themselves
pub use rage_formats::{NameTable, Vec3};
use rage_formats::{build_meta, build_pso, dump_meta, dump_pso, is_pso, rage_joaat, to_xml, value::MetaStruct, MetaValue, Schema, Vec4};
use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

/// What one entity says, read from the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityView {
    pub archetype_hash: u32,
    pub position: Vec3,
    /// quaternion x, y, z, w
    pub rotation: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
    pub flags: u32,
    pub guid: u32,
    /// index of the LOD parent entity in this file, -1 for none
    pub parent_index: i32,
    pub num_children: u32,
    pub lod_dist: f32,
    pub child_lod_dist: f32,
    /// `CMloInstanceDef` rather than `CEntityDef`
    pub is_mlo_instance: bool,
    /// `rage__eLodType`: 0 HD, 1 LOD, 2 SLOD1, 3 SLOD2, 4 SLOD3, 5 orphan HD, 6 SLOD4
    pub lod_level: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sev {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct MapIssue {
    pub sev: Sev,
    pub message: String,
    pub entity: Option<usize>,
}

/// What the extents need to know about an archetype.
#[derive(Debug, Clone, Copy)]
pub struct ArchBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// the archetype's own LOD distance, used by an entity whose `lodDist` is 0 or less
    pub lod_dist: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentsMode {
    /// only make the extents bigger where an entity now reaches past them (never smaller than the file had)
    GrowOnly,
    /// compute them again from every entity
    Recalculate,
}

pub struct YmapDocument {
    pub root: MetaValue,
    /// RSC7 Meta (nearly all `.ymap`) or PSO (a few DLC ones)
    pub container: Container,
    pub path: Option<PathBuf>,
    pub read_warnings: Vec<String>,
    pub names: NameTable,
    schema: Schema,
    history: History,
}

fn entities_path() -> Path {
    vec![field("entities")]
}

fn entity_path(i: usize, member: &str) -> Path {
    vec![field("entities"), Step::Index(i), field(member)]
}

impl YmapDocument {
    /// A new, empty map called `name`: every structure a retail `.ymap` carries, empty, so tools that expect them find them.
    pub fn new_map(name: &str) -> Result<Self> {
        let name = name.trim();
        if name.is_empty() || name.contains(|c: char| c.is_whitespace() || "/\\<>&\"".contains(c)) {
            bail!("a map name has no spaces or slashes");
        }
        let xml = format!(
            r#"<CMapData>
  <name>{name}</name><parent/><flags value="0"/><contentFlags value="0"/>
  <streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/>
  <entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/>
  <entities/>
  <containerLods itemType="rage__fwContainerLodDef"/><boxOccluders itemType="BoxOccluder"/><occludeModels itemType="OccludeModel"/>
  <physicsDictionaries/>
  <instancedData><ImapLink/><PropInstanceList itemType="rage__fwPropInstanceListDef"/><GrassInstanceList itemType="rage__fwGrassInstanceListDef"/></instancedData>
  <timeCycleModifiers itemType="CTimeCycleModifier"/><carGenerators itemType="CCarGen"/>
  <LODLightsSOA><direction/><falloff/><falloffExponent/><timeAndStateFlags/><hash/><coneInnerAngle/><coneOuterAngleOrCapExt/><coronaIntensity/></LODLightsSOA>
  <DistantLODLightsSOA><position/><RGBI/><numStreetLights value="0"/><category value="0"/></DistantLODLightsSOA>
  <block><version value="0"/><flags value="0"/><name>{name}</name><exportedBy>CRCK Explorer</exportedBy><owner/><time/></block>
</CMapData>"#
        );
        let mut root = rage_formats::from_xml(&xml).context("the new map template")?;
        schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
        let schema = Schema::builtin().clone();
        let built = build_meta(&root, &schema).context("building the new map")?;
        let mut d = Self::open(&built.bytes, None)?;
        d.names.add_lowercase(name);
        Ok(d)
    }

    /// Set the flags the contents decide ([`YmapDocument::update_flags`]). True when they changed.
    pub fn update_content_flags(&mut self) -> Result<bool> {
        self.update_flags()
    }

    /// Open the bytes of a `.ymap` (RSC7 Meta).
    pub fn open(data: &[u8], path: Option<PathBuf>) -> Result<Self> {
        let mut schema = Schema::builtin().clone();
        if let Ok(own) = Schema::from_file(data) {
            schema.merge(own);
        }
        let (d, container) = if is_pso(data) { (dump_pso(data).context("reading the PSO file")?, Container::Pso) } else { (dump_meta(data).context("reading the file")?, Container::Meta) };
        match d.root.as_struct() {
            Some(s) if s.type_hash == rage_joaat("CMapData") => {}
            _ => bail!("this is not a map file: its root is not CMapData"),
        }
        Ok(Self { root: d.root, container, path, read_warnings: d.warnings, names: NameTable::core(), schema, history: History::default() })
    }

    pub fn open_file(path: &FsPath) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::open(&data, Some(path.to_path_buf()))
    }

    pub fn entities(&self) -> &[MetaValue] {
        crate::archetype::edit::get(&self.root, &entities_path()).map(|v| v.items()).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.entities().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn entity(&self, i: usize) -> Option<EntityView> {
        let v = self.entities().get(i)?;
        let s = v.as_struct()?;
        let q = match s.field("rotation") {
            Some(MetaValue::Vec4(q)) => [q.x, q.y, q.z, q.w],
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        Some(EntityView {
            archetype_hash: s.field("archetypeName")?.as_hash()?,
            position: s.field("position")?.as_vec3()?,
            rotation: q,
            scale_xy: s.field("scaleXY").and_then(|v| v.as_f32()).unwrap_or(1.0),
            scale_z: s.field("scaleZ").and_then(|v| v.as_f32()).unwrap_or(1.0),
            flags: s.field("flags").and_then(|v| v.as_u32()).unwrap_or(0),
            guid: s.field("guid").and_then(|v| v.as_u32()).unwrap_or(0),
            parent_index: s.field("parentIndex").and_then(|v| v.as_i64()).unwrap_or(-1) as i32,
            num_children: s.field("numChildren").and_then(|v| v.as_u32()).unwrap_or(0),
            lod_dist: s.field("lodDist").and_then(|v| v.as_f32()).unwrap_or(0.0),
            child_lod_dist: s.field("childLodDist").and_then(|v| v.as_f32()).unwrap_or(0.0),
            is_mlo_instance: s.type_hash == rage_joaat("CMloInstanceDef"),
            lod_level: match s.field("lodLevel") {
                Some(MetaValue::Enum { value, .. }) => (*value).clamp(0, 255) as u8,
                _ => 0,
            },
        })
    }

    pub fn entity_name(&self, i: usize) -> String {
        self.entity(i).map(|e| self.names.resolve(e.archetype_hash).into_owned()).unwrap_or_default()
    }

    /// the map's own name (`name`), as text
    pub fn name(&self) -> String {
        let h = self.root.as_struct().and_then(|s| s.field("name")).and_then(|v| v.as_hash()).unwrap_or(0);
        self.names.resolve(h).into_owned()
    }

    // ---- editing --------------------------------------------------------------------------------------------------------

    pub fn run(&mut self, cmd: Command, label: &str, key: Option<String>) -> Result<()> {
        self.history.run(&mut self.root, cmd, label, key)
    }

    pub fn undo(&mut self) -> Result<()> {
        self.history.undo(&mut self.root)
    }

    pub fn redo(&mut self) -> Result<()> {
        self.history.redo(&mut self.root)
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    fn need(&self, i: usize) -> Result<()> {
        if i >= self.len() {
            bail!("no entity {i}: the file has {}", self.len());
        }
        Ok(())
    }

    /// Move entity `i`. Repeated moves of the same entity (a drag) are one undo step.
    pub fn set_position(&mut self, i: usize, p: Vec3) -> Result<()> {
        self.need(i)?;
        self.run(Command::Set(entity_path(i, "position"), MetaValue::Vec3(p)), "Move entity", Some(format!("pos{i}")))
    }

    /// Rotate entity `i` to the quaternion `q` (x, y, z, w), which must be non-zero; it is normalised.
    pub fn set_rotation(&mut self, i: usize, q: [f32; 4]) -> Result<()> {
        self.need(i)?;
        let len = q.iter().map(|c| c * c).sum::<f32>().sqrt();
        if !len.is_finite() || len < 1e-6 {
            bail!("a rotation cannot be zero");
        }
        let q = Vec4::new(q[0] / len, q[1] / len, q[2] / len, q[3] / len);
        self.run(Command::Set(entity_path(i, "rotation"), MetaValue::Vec4(q)), "Rotate entity", Some(format!("rot{i}")))
    }

    /// Set the quaternion exactly as given (already the stored form, which a project keeps); no normalising.
    pub fn set_rotation_exact(&mut self, i: usize, q: [f32; 4]) -> Result<()> {
        self.need(i)?;
        self.run(Command::Set(entity_path(i, "rotation"), MetaValue::Vec4(Vec4::new(q[0], q[1], q[2], q[3]))), "Rotate entity", Some(format!("rot{i}")))
    }

    pub fn set_scale(&mut self, i: usize, xy: f32, z: f32) -> Result<()> {
        self.need(i)?;
        if !(xy.is_finite() && z.is_finite()) || xy <= 0.0 || z <= 0.0 {
            bail!("a scale must be above zero");
        }
        self.run(
            Command::Batch(vec![Command::Set(entity_path(i, "scaleXY"), MetaValue::F32(xy)), Command::Set(entity_path(i, "scaleZ"), MetaValue::F32(z))]),
            "Scale entity",
            Some(format!("scale{i}")),
        )
    }

    /// A new `CEntityDef` for `archetype` at `position`, added at the end (no LOD parent, no children). Returns its index.
    pub fn add_entity(&mut self, archetype: &str, position: Vec3) -> Result<usize> {
        let archetype = archetype.trim();
        if archetype.is_empty() {
            bail!("the entity needs an archetype name");
        }
        let at = self.len();
        let item = self.new_entity_value(archetype, position, at);
        self.names.add_lowercase(archetype);
        self.run(Command::Insert(entities_path(), at, item), "Add entity", None)?;
        Ok(at)
    }

    /// A copy of entity `i` (with a new guid) inserted at the end, as a stand-alone entity: it does not become a LOD child or
    /// parent of anything. Returns the copy's index.
    pub fn duplicate_entity(&mut self, i: usize) -> Result<usize> {
        self.need(i)?;
        let mut copy = self.entities()[i].clone();
        let at = self.len();
        let guid = self.fresh_guid(at as u32 ^ self.entity(i).map_or(0, |e| e.guid));
        if let MetaValue::Struct(s) = &mut copy {
            for (h, v) in s.fields.iter_mut() {
                if *h == rage_joaat("guid") {
                    *v = MetaValue::U32(guid);
                } else if *h == rage_joaat("parentIndex") {
                    *v = MetaValue::I32(-1);
                } else if *h == rage_joaat("numChildren") {
                    *v = MetaValue::U32(0);
                }
            }
        }
        self.run(Command::Insert(entities_path(), at, copy), "Duplicate entity", None)?;
        Ok(at)
    }

    /// Remove entity `i`. Refused while another entity names it as its LOD parent (that would orphan them: unlink or remove those
    /// first). The parent indices of the entities after it move down by one, and its own parent loses a child; all of it one undo step.
    pub fn remove_entity(&mut self, i: usize) -> Result<()> {
        self.need(i)?;
        self.remove_entities(&[i])
    }

    /// A `CEntityDef` with every member of the game's table at its empty value, then what a plain placement needs.
    pub(crate) fn new_entity_value(&self, archetype: &str, position: Vec3, nth: usize) -> MetaValue {
        self.new_typed_entity_value(archetype, position, nth, "CEntityDef")
    }

    /// [`YmapDocument::new_entity_value`] of structure `ty` (`CEntityDef`, or `CMloInstanceDef` for an interior)
    pub(crate) fn new_typed_entity_value(&self, archetype: &str, position: Vec3, nth: usize, ty: &str) -> MetaValue {
        let type_hash = rage_joaat(ty);
        let h = schema_view::name_hash(archetype);
        let mut fields: Vec<(u32, MetaValue)> = vec![];
        for f in schema_view::describe(type_hash).unwrap_or_default() {
            let mut v = schema_view::default_value(&f.kind);
            match f.name.as_str() {
                "archetypeName" => v = MetaValue::Hash(h),
                "position" => v = MetaValue::Vec3(position),
                "rotation" => v = MetaValue::Vec4(Vec4::new(0.0, 0.0, 0.0, 1.0)),
                "scaleXY" | "scaleZ" => v = MetaValue::F32(1.0),
                "parentIndex" => v = MetaValue::I32(-1),
                "lodDist" => v = MetaValue::F32(100.0),
                "guid" => v = MetaValue::U32(self.fresh_guid((nth as u32).wrapping_add(h))),
                "ambientOcclusionMultiplier" | "artificialAmbientOcclusion" => v = MetaValue::U8(255),
                _ => {}
            }
            fields.push((f.hash, v));
        }
        MetaValue::Struct(MetaStruct { type_hash, fields })
    }

    // ---- checks ---------------------------------------------------------------------------------------------------------

    /// Problems that only show up in game. `has_archetype` says whether a name hash is a known model (pass `None` to skip that check).
    pub fn check(&self, has_archetype: Option<&dyn Fn(u32) -> bool>) -> Vec<MapIssue> {
        let mut out = vec![];
        let n = self.len();
        // the extents are the boxes of the entities' models, so a position may lie outside by up to a model's radius; 1.0 flagged 1,022 of
        // the game's own files
        const SLACK: f32 = 100.0;
        let (lo, hi) = (self.extent("entitiesExtentsMin"), self.extent("entitiesExtentsMax"));
        let mut kids = vec![0u32; n];
        for i in 0..n {
            let Some(e) = self.entity(i) else {
                out.push(MapIssue { sev: Sev::Error, message: "this entity has no archetype name or position".into(), entity: Some(i) });
                continue;
            };
            // with the "LOD parent is in the parent .ymap" flag the index points into another file: nothing to check here
            if e.flags & (1 << 3) != 0 {
            } else if e.parent_index >= n as i32 || e.parent_index < -1 || e.parent_index == i as i32 {
                out.push(MapIssue { sev: Sev::Error, message: format!("its LOD parent index {} is not an entity of this file", e.parent_index), entity: Some(i) });
            } else if e.parent_index >= 0 {
                kids[e.parent_index as usize] += 1;
            }
            let len = e.rotation.iter().map(|c| c * c).sum::<f32>().sqrt();
            if (len - 1.0).abs() > 0.01 {
                out.push(MapIssue { sev: Sev::Warning, message: format!("its rotation is not a unit quaternion (length {len:.3})"), entity: Some(i) });
            }
            if let (Some(lo), Some(hi)) = (lo, hi) {
                let p = e.position;
                if p.x < lo.x - SLACK || p.y < lo.y - SLACK || p.z < lo.z - SLACK || p.x > hi.x + SLACK || p.y > hi.y + SLACK || p.z > hi.z + SLACK {
                    out.push(MapIssue { sev: Sev::Warning, message: "it lies outside entitiesExtents, which this tool does not recompute: the game may not stream it in".into(), entity: Some(i) });
                }
            }
            if let Some(has) = has_archetype {
                if !has(e.archetype_hash) && !e.is_mlo_instance {
                    out.push(MapIssue { sev: Sev::Warning, message: format!("no model or archetype named {} was found", self.names.resolve(e.archetype_hash)), entity: Some(i) });
                }
            }
        }
        for i in 0..n {
            if let Some(e) = self.entity(i) {
                // children may also live in other files, so only too few is wrong
                if e.num_children < kids[i] {
                    out.push(MapIssue { sev: Sev::Warning, message: format!("it says it has {} LOD children but {} entities of this file name it as their parent", e.num_children, kids[i]), entity: Some(i) });
                }
            }
        }
        out
    }

    /// (entitiesExtentsMin, entitiesExtentsMax, streamingExtentsMin, streamingExtentsMax) worked out from the contents
    /// ([`YmapDocument::calc_extents`]), or `None` when nothing contributes.
    pub fn computed_extents(&self, bounds: &dyn Fn(u32) -> Option<ArchBounds>) -> Option<([f32; 3], [f32; 3], [f32; 3], [f32; 3])> {
        self.calc_extents(bounds).map(|e| (e.entities_min, e.entities_max, e.streaming_min, e.streaming_max))
    }

    /// Set the four extents boxes (see [`YmapDocument::computed_extents`]); one undo step. True when anything changed.
    pub fn update_extents(&mut self, bounds: &dyn Fn(u32) -> Option<ArchBounds>, mode: ExtentsMode) -> Result<bool> {
        let Some((emin, emax, smin, smax)) = self.computed_extents(bounds) else { return Ok(false) };
        let v = |a: [f32; 3]| glam::Vec3::from(a);
        let old = |n: &str| self.extent(n).map(|p| glam::Vec3::new(p.x, p.y, p.z));
        let pick = |name: &str, new: [f32; 3], grow_max: bool| -> Vec3 {
            let n = v(new);
            let r = match (mode, old(name)) {
                (ExtentsMode::GrowOnly, Some(o)) => if grow_max { o.max(n) } else { o.min(n) },
                _ => n,
            };
            Vec3::new(r.x, r.y, r.z)
        };
        let sets = [("entitiesExtentsMin", pick("entitiesExtentsMin", emin, false)), ("entitiesExtentsMax", pick("entitiesExtentsMax", emax, true)), ("streamingExtentsMin", pick("streamingExtentsMin", smin, false)), ("streamingExtentsMax", pick("streamingExtentsMax", smax, true))];
        let cmds: Vec<Command> = sets.iter().filter(|(n, val)| self.extent(n) != Some(*val)).map(|(n, val)| Command::Set(vec![field(n)], MetaValue::Vec3(*val))).collect();
        if cmds.is_empty() {
            return Ok(false);
        }
        self.run(Command::Batch(cmds), "Update extents", None)?;
        Ok(true)
    }

    pub(crate) fn extent(&self, name: &str) -> Option<Vec3> {
        self.root.as_struct()?.field(name)?.as_vec3()
    }

    // ---- saving ---------------------------------------------------------------------------------------------------------

    /// The file's bytes, built with the Meta writer, read back and compared with the tree: an error says what would be lost.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let built = match self.container {
            Container::Meta => build_meta(&self.root, &self.schema),
            Container::Pso => build_pso(&self.root, &self.schema),
        }
        .context("building the file")?;
        if let Some(w) = built.warnings.first() {
            bail!("the writer cannot save part of this file: {w}");
        }
        let back = match self.container {
            Container::Meta => dump_meta(&built.bytes),
            Container::Pso => dump_pso(&built.bytes),
        }
        .context("reading the saved bytes back")?;
        let (diffs, total) = compare_trees(&self.root, &back.root, &self.names);
        if let Some(first) = diffs.first() {
            bail!("saving would change the file: {total} value(s) differ, the first at {} ({} became {}); nothing was written", first.path, first.before, first.after);
        }
        Ok(built.bytes)
    }

    pub fn to_xml(&self) -> String {
        to_xml(&self.root, &self.names)
    }

    /// Save to `path`; the first overwrite keeps the old file as `.bak`, the new one is written beside it and moved over.
    pub fn save(&mut self, path: &FsPath) -> Result<()> {
        self.write_to(path, true)
    }

    /// Write the file (built, read back and compared first) to `path` through a temporary file; with `keep_backup`, the first overwrite
    /// leaves the old file as `.bak`. Marks the document as saved.
    pub fn write_to(&mut self, path: &FsPath, keep_backup: bool) -> Result<()> {
        let bytes = self.to_bytes()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        if keep_backup && path.exists() {
            let bak = PathBuf::from(format!("{}.bak", path.display()));
            if !bak.exists() {
                fs::copy(path, &bak).with_context(|| format!("keeping a backup of {}", path.display()))?;
            }
        }
        let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", path.display()));
        fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.history.mark_saved();
        Ok(())
    }

    /// The next edit is a new undo step even if it has the same key as the last one (a drag or a typed number has ended).
    pub fn end_edit_group(&mut self) {
        self.history.break_join();
    }

    /// How many undo steps there are (it only grows when an edit is a new step, not when it joins the last one).
    pub fn undo_steps(&self) -> usize {
        self.history.steps()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a small `.ymap` made through the same writer: three entities, the last one a LOD child of the first
    fn sample() -> YmapDocument {
        let xml = r#"<CMapData>
  <name>test_map</name>
  <parent/>
  <flags value="0"/>
  <contentFlags value="1"/>
  <streamingExtentsMin x="-10" y="-10" z="-10"/>
  <streamingExtentsMax x="10" y="10" z="10"/>
  <entitiesExtentsMin x="-5" y="-5" z="-5"/>
  <entitiesExtentsMax x="5" y="5" z="5"/>
  <entities>
    <Item type="CEntityDef"><archetypeName>prop_a</archetypeName><flags value="0"/><guid value="1"/><position x="0" y="0" z="0"/><rotation x="0" y="0" z="0" w="1"/><scaleXY value="1"/><scaleZ value="1"/><parentIndex value="-1"/><lodDist value="100"/><childLodDist value="0"/><lodLevel>LODTYPES_DEPTH_HD</lodLevel><numChildren value="1"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>
    <Item type="CEntityDef"><archetypeName>prop_b</archetypeName><flags value="0"/><guid value="2"/><position x="1" y="1" z="1"/><rotation x="0" y="0" z="0" w="1"/><scaleXY value="1"/><scaleZ value="1"/><parentIndex value="-1"/><lodDist value="100"/><childLodDist value="0"/><lodLevel>LODTYPES_DEPTH_HD</lodLevel><numChildren value="0"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>
    <Item type="CEntityDef"><archetypeName>prop_c</archetypeName><flags value="0"/><guid value="3"/><position x="2" y="2" z="2"/><rotation x="0" y="0" z="0" w="1"/><scaleXY value="1"/><scaleZ value="1"/><parentIndex value="0"/><lodDist value="50"/><childLodDist value="0"/><lodLevel>LODTYPES_DEPTH_LOD</lodLevel><numChildren value="0"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>
  </entities>
</CMapData>"#;
        let mut root = rage_formats::from_xml(xml).unwrap();
        schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
        let schema = Schema::builtin().clone();
        let bytes = build_meta(&root, &schema).unwrap().bytes;
        let mut d = YmapDocument::open(&bytes, None).unwrap();
        for n in ["test_map", "prop_a", "prop_b", "prop_c", "prop_new"] {
            d.names.add_lowercase(n);
        }
        d
    }

    #[test]
    fn reads_entities_and_rejects_other_files() {
        let d = sample();
        assert_eq!((d.len(), d.name().as_str()), (3, "test_map"));
        let e = d.entity(2).unwrap();
        assert_eq!((e.parent_index, e.lod_dist), (0, 50.0));
        assert_eq!(d.entity(1).unwrap().position, Vec3::new(1.0, 1.0, 1.0));
        let types = rage_toolkit_ytyp_bytes();
        assert!(YmapDocument::open(&types, None).err().unwrap().to_string().contains("not a map file"));
        assert!(d.check(None).is_empty(), "{:?}", d.check(None));
    }

    fn rage_toolkit_ytyp_bytes() -> Vec<u8> {
        crate::archetype::document::YtypDocument::new_file("x").to_bytes().unwrap()
    }

    #[test]
    fn edits_undo_and_save_reopen() {
        let mut d = sample();
        d.set_position(1, Vec3::new(3.0, 4.0, 5.0)).unwrap();
        d.set_position(1, Vec3::new(3.5, 4.0, 5.0)).unwrap();
        d.set_rotation(1, [0.0, 0.0, 2.0, 2.0]).unwrap();
        d.set_scale(1, 2.0, 3.0).unwrap();
        let i = d.add_entity("prop_new", Vec3::new(1.0, 2.0, 3.0)).unwrap();
        assert_eq!((i, d.len()), (3, 4));
        let e = d.entity(1).unwrap();
        assert_eq!((e.position.x, e.scale_xy, e.scale_z), (3.5, 2.0, 3.0));
        assert!((e.rotation[2] - 0.70710677).abs() < 1e-6 && (e.rotation[3] - 0.70710677).abs() < 1e-6);
        assert_eq!(d.entity(3).unwrap().parent_index, -1);
        // the two moves were one undo step
        d.undo().unwrap();
        d.undo().unwrap();
        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!(d.entity(1).unwrap().position, Vec3::new(1.0, 1.0, 1.0));
        d.redo().unwrap();
        d.redo().unwrap();
        d.redo().unwrap();
        d.redo().unwrap();
        let file = std::env::temp_dir().join(format!("crck_ymap_test_{}.ymap", std::process::id()));
        d.save(&file).unwrap();
        let mut back = YmapDocument::open_file(&file).unwrap();
        back.names.add_lowercase("prop_new");
        let _ = fs::remove_file(&file);
        let _ = fs::remove_file(format!("{}.bak", file.display()));
        assert_eq!(back.len(), 4);
        assert_eq!(back.entity(1).unwrap(), d.entity(1).unwrap());
        assert_eq!(back.entity_name(3), "prop_new");
        assert!(!d.is_dirty());
    }

    #[test]
    fn removing_keeps_lod_links_straight_and_refuses_parents() {
        let mut d = sample();
        let err = d.remove_entity(0).unwrap_err().to_string();
        assert!(err.contains("LOD parent"), "{err}");
        d.remove_entity(1).unwrap();
        // entity 2 became entity 1; its parent (0) is unchanged
        assert_eq!((d.len(), d.entity(1).unwrap().parent_index), (2, 0));
        d.remove_entity(1).unwrap();
        assert_eq!(d.entity(0).unwrap().num_children, 0, "the parent lost its child");
        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!((d.len(), d.entity(0).unwrap().num_children, d.entity(2).unwrap().parent_index), (3, 1, 0));
    }

    #[test]
    fn a_new_map_takes_entities_saves_and_sets_its_flags() {
        let mut d = YmapDocument::new_map("my_map").unwrap();
        assert_eq!((d.len(), d.name().as_str()), (0, "my_map"));
        assert!(YmapDocument::new_map("bad name").is_err());
        d.add_entity("prop_a", Vec3::new(1.0, 2.0, 3.0)).unwrap();
        assert!(d.update_content_flags().unwrap(), "an HD entity sets the HD bit");
        assert_eq!(d.root.as_struct().and_then(|s| s.field("contentFlags")).and_then(|v| v.as_u32()), Some(1));
        let b = |_: u32| Some(ArchBounds { min: [-1.0; 3], max: [1.0; 3], lod_dist: 100.0 });
        d.update_extents(&b, ExtentsMode::Recalculate).unwrap();
        let bytes = d.to_bytes().unwrap();
        let mut back = YmapDocument::open(&bytes, None).unwrap();
        back.names.add_lowercase("prop_a");
        assert_eq!((back.len(), back.entity_name(0)), (1, "prop_a".to_string()));
        assert_eq!(back.entity(0).unwrap().position, Vec3::new(1.0, 2.0, 3.0));
        assert!(!back.update_content_flags().unwrap(), "already right");
    }

    #[test]
    fn extents_follow_the_entities_and_only_grow_when_asked_to() {
        let mut d = sample();
        let b = |h: u32| if h == rage_joaat("prop_a") { Some(ArchBounds { min: [-1.0, -1.0, -1.0], max: [1.0, 1.0, 1.0], lod_dist: 100.0 }) } else { None };
        // prop_a at the origin spans -1..1; prop_b and prop_c are at (1,1,1) and (2,2,2) with no bounds: points. lodDist 100, 100, 50
        let (emin, emax, smin, smax) = d.computed_extents(&b).unwrap();
        assert_eq!((emin, emax), ([-1.0, -1.0, -1.0], [2.0, 2.0, 2.0]));
        assert_eq!((smin, smax), ([-101.0, -101.0, -101.0], [101.0, 101.0, 101.0]));
        // the file said -5..5: growing keeps that, recalculating shrinks it
        assert!(!d.update_extents(&b, ExtentsMode::GrowOnly).unwrap() || d.extent("entitiesExtentsMin") == Some(Vec3::new(-5.0, -5.0, -5.0)));
        assert_eq!(d.extent("entitiesExtentsMin"), Some(Vec3::new(-5.0, -5.0, -5.0)));
        assert_eq!(d.extent("streamingExtentsMax"), Some(Vec3::new(101.0, 101.0, 101.0)), "the streaming box had to grow to the lod distance");
        d.set_position(2, Vec3::new(50.0, 0.0, 0.0)).unwrap();
        d.update_extents(&b, ExtentsMode::GrowOnly).unwrap();
        assert_eq!(d.extent("entitiesExtentsMax"), Some(Vec3::new(50.0, 5.0, 5.0)));
        d.update_extents(&b, ExtentsMode::Recalculate).unwrap();
        assert_eq!((d.extent("entitiesExtentsMin"), d.extent("entitiesExtentsMax")), (Some(Vec3::new(-1.0, -1.0, -1.0)), Some(Vec3::new(50.0, 1.0, 1.0))));
        d.undo().unwrap();
        d.undo().unwrap();
        d.undo().unwrap();
        assert_eq!(d.extent("entitiesExtentsMax"), Some(Vec3::new(5.0, 5.0, 5.0)));
    }

    #[test]
    fn checks_find_broken_links_and_far_entities() {
        let mut d = sample();
        d.set_position(1, Vec3::new(500.0, 0.0, 0.0)).unwrap();
        d.run(Command::Set(entity_path(2, "parentIndex"), MetaValue::I32(9)), "x", None).unwrap();
        let issues = d.check(Some(&|h| h != rage_joaat("prop_c")));
        let text: Vec<String> = issues.iter().map(|i| format!("{:?} {}", i.entity, i.message)).collect();
        assert!(text.iter().any(|t| t.contains("outside entitiesExtents")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("parent index 9")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("no model or archetype named prop_c")), "{text:?}");
        d.run(Command::Set(entity_path(2, "parentIndex"), MetaValue::I32(0)), "x", None).unwrap();
        d.run(Command::Set(entity_path(0, "numChildren"), MetaValue::U32(0)), "x", None).unwrap();
        let text: Vec<String> = d.check(None).iter().map(|i| i.message.clone()).collect();
        assert!(text.iter().any(|t| t.contains("says it has 0 LOD children but 1")), "{text:?}");
        // a parent in another file (the flag) is not checked here
        d.run(Command::Set(entity_path(2, "flags"), MetaValue::U32(8)), "x", None).unwrap();
        d.run(Command::Set(entity_path(2, "parentIndex"), MetaValue::I32(77)), "x", None).unwrap();
        assert!(d.check(None).iter().all(|i| !i.message.contains("parent index")));
    }

    #[test]
    fn damaged_files_are_refused_and_never_make_the_reader_panic() {
        // things that are not map files at all
        let not_files: Vec<Vec<u8>> = vec![vec![], b"x".to_vec(), b"RSC7".to_vec(), b"RSC7\0\0\0\0".to_vec(), vec![0; 16], vec![0xFF; 4096], b"<CMapData></CMapData>".to_vec(), b"PSIN\0\0\0\0\0\0\0\0".to_vec()];
        for junk in &not_files {
            assert!(YmapDocument::open(junk, None).is_err(), "{:?} should be refused", &junk[..junk.len().min(8)]);
        }
        // a good file, cut short at many lengths and with bytes changed: each outcome is a map or a message, never a panic
        let mut d = YmapDocument::new_map("fuzz").unwrap();
        for i in 0..5 {
            d.add_entity(&format!("prop_{i}"), Vec3::new(i as f32, 0.0, 0.0)).unwrap();
        }
        let bytes = d.to_bytes().unwrap();
        assert_eq!(YmapDocument::open(&bytes, None).unwrap().len(), 5, "the intact file reads back");
        for cut in (0..bytes.len()).step_by(3) {
            let _ = YmapDocument::open(&bytes[..cut], None);
        }
        let mut state = 12345u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        let mut refused = 0;
        for _ in 0..600 {
            let mut b = bytes.clone();
            for _ in 0..1 + next() % 4 {
                let at = next() as usize % b.len();
                b[at] ^= (next() % 255 + 1) as u8;
            }
            if YmapDocument::open(&b, None).is_err() {
                refused += 1;
            }
        }
        assert!(refused > 100, "most damaged files must be refused, not read as something else: {refused} of 600");
        // trailing bytes after a good file are ignored, as the game does
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0; 64]);
        let _ = YmapDocument::open(&padded, None);
    }

    #[test]
    fn a_failed_save_leaves_the_old_file_alone_and_the_document_unsaved() {
        let dir = std::env::temp_dir().join(format!("crck_ymap_failed_save_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut d = sample();
        let file = dir.join("a.ymap");
        d.write_to(&file, false).unwrap();
        let good = fs::read(&file).unwrap();
        d.set_position(0, Vec3::new(1.0, 2.0, 3.0)).unwrap();
        assert!(d.is_dirty());
        // somewhere that cannot be written (below a file)
        assert!(d.write_to(&file.join("sub").join("b.ymap"), false).is_err());
        assert!(d.is_dirty(), "a failed save is not a save");
        assert_eq!(fs::read(&file).unwrap(), good, "the old file is as it was");
        // the save that works marks the document clean, keeps the first version as .bak and leaves no temporary file
        d.write_to(&file, true).unwrap();
        assert!(!d.is_dirty() && dir.join("a.ymap.bak").exists());
        assert_eq!(fs::read(dir.join("a.ymap.bak")).unwrap(), good);
        assert!(!dir.join("a.ymap.rage-toolkit-tmp").exists());
        assert_eq!(YmapDocument::open_file(&file).unwrap().entity(0).unwrap().position, Vec3::new(1.0, 2.0, 3.0));
        let _ = fs::remove_dir_all(&dir);
    }
}
