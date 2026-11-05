//! The LOD hierarchy of a `.ymap`: which entity is another's LOD parent, who the roots are, and what is wrong. The rules: an entity is a root when its `parentIndex` is below 0 or past the end, when its flags say the parent is in
//! the parent map (`LodInParentYmap`, bit 3), or when the parent is not of a higher LOD level than the child, or when the parent is an
//! orphan HD entity and the child is not. A `parentIndex` that points somewhere is therefore **not** proof of a valid relationship.
//!
//! Levels (`rage__eLodType`): 0 HD, 1 LOD, 2 SLOD1, 3 SLOD2, 4 SLOD3, 5 orphan HD, 6 SLOD4.
use crate::{
    archetype::edit::{field, Command, Step},
    ymap_doc::YmapDocument,
};
use anyhow::{bail, Result};
use rage_formats::MetaValue;

pub const LOD_ORPHANHD: u8 = 5;
const LOD_IN_PARENT_YMAP: u32 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodIssueKind {
    /// `parentIndex` is 0 or more but there is no such entity (and the parent is not said to be in another map)
    ParentOutOfRange,
    ParentIsItself,
    /// the parent's level is not higher than the child's
    ParentNotHigher,
    /// an orphan HD entity is the parent of one that is not orphan HD
    OrphanParentMismatch,
    /// following the parent indices comes back to the start
    Cycle,
    /// `numChildren` is less than the number of entities of this map that name it as parent
    ChildCountTooLow,
    /// the parent index points into the parent map (which this file alone cannot check)
    ParentInOtherMap,
}

#[derive(Debug, Clone)]
pub struct LodIssue {
    pub entity: usize,
    pub kind: LodIssueKind,
    pub message: String,
}

impl LodIssue {
    /// whether this is a defect (as opposed to a note)
    pub fn is_error(&self) -> bool {
        !matches!(self.kind, LodIssueKind::ParentInOtherMap)
    }
}

/// How [`YmapDocument::rebuild_hierarchy`] sets `numChildren`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountMode {
    /// only raise a count that is lower than the children of this file (more children may live in other files, which are not seen here)
    RaiseOnly,
    /// set every count to the children of this file (when no other file has children of these entities)
    Exact,
}

/// What [`YmapDocument::rebuild_hierarchy`] changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RebuildReport {
    /// entities whose LOD parent was not a valid relationship and was removed: they are roots now
    pub detached: Vec<usize>,
    /// entities whose `numChildren` was changed
    pub counts_changed: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Hierarchy {
    /// each entity's valid parent in this file, `None` for roots
    pub parent: Vec<Option<usize>>,
    pub children: Vec<Vec<usize>>,
    pub roots: Vec<usize>,
    pub issues: Vec<LodIssue>,
}

impl Hierarchy {
    /// the chain of valid parents above `i`, nearest first
    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut cur = self.parent.get(i).copied().flatten();
        while let Some(p) = cur {
            if out.contains(&p) || out.len() > self.parent.len() {
                break;
            }
            out.push(p);
            cur = self.parent[p];
        }
        out
    }

    /// everything below `i`, in index order
    pub fn descendants(&self, i: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            for &c in self.children.get(n).map_or(&[][..], |v| v.as_slice()) {
                if !out.contains(&c) {
                    out.push(c);
                    stack.push(c);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

struct Raw {
    level: u8,
    parent_index: i32,
    in_parent_ymap: bool,
    num_children: u32,
}

impl YmapDocument {
    fn raw_lod(&self) -> Vec<Raw> {
        (0..self.len())
            .map(|i| {
                let e = self.entity(i);
                Raw { level: e.as_ref().map_or(0, |e| e.lod_level), parent_index: e.as_ref().map_or(-1, |e| e.parent_index), in_parent_ymap: e.as_ref().map_or(false, |e| e.flags & LOD_IN_PARENT_YMAP != 0), num_children: e.as_ref().map_or(0, |e| e.num_children) }
            })
            .collect()
    }

    /// Why a relationship child -> parent would not hold (`None` when it is fine).
    fn relation_problem(raw: &[Raw], child: usize, parent: usize) -> Option<(LodIssueKind, String)> {
        if parent == child {
            return Some((LodIssueKind::ParentIsItself, "an entity cannot be its own LOD parent".into()));
        }
        let (c, p) = (&raw[child], &raw[parent]);
        if p.level <= c.level && !(p.level == LOD_ORPHANHD) {
            return Some((LodIssueKind::ParentNotHigher, format!("the parent (entity {parent}) is not of a higher LOD level than the child (level {} against {})", p.level, c.level)));
        }
        if p.level == LOD_ORPHANHD && c.level != LOD_ORPHANHD {
            return Some((LodIssueKind::OrphanParentMismatch, format!("the parent (entity {parent}) is an orphan HD entity, which only orphan HD entities can be the parent of")));
        }
        None
    }

    /// Build the hierarchy and list what is wrong with it.
    pub fn hierarchy(&self) -> Hierarchy {
        let raw = self.raw_lod();
        let n = raw.len();
        let mut h = Hierarchy { parent: vec![None; n], children: vec![vec![]; n], roots: vec![], issues: vec![] };
        for i in 0..n {
            let r = &raw[i];
            if r.in_parent_ymap {
                if r.parent_index >= 0 {
                    h.issues.push(LodIssue { entity: i, kind: LodIssueKind::ParentInOtherMap, message: format!("its LOD parent is entity {} of the parent map", r.parent_index) });
                }
                h.roots.push(i);
                continue;
            }
            if r.parent_index < 0 {
                h.roots.push(i);
                continue;
            }
            if r.parent_index as usize >= n {
                h.issues.push(LodIssue { entity: i, kind: LodIssueKind::ParentOutOfRange, message: format!("its LOD parent index {} is past the last entity ({})", r.parent_index, n.saturating_sub(1)) });
                h.roots.push(i);
                continue;
            }
            let p = r.parent_index as usize;
            match Self::relation_problem(&raw, i, p) {
                Some((kind, message)) => {
                    h.issues.push(LodIssue { entity: i, kind, message });
                    h.roots.push(i);
                }
                None => {
                    h.parent[i] = Some(p);
                    h.children[p].push(i);
                }
            }
        }
        // a loop in the raw indices (even through relations the rules reject) would be a data fault of its own
        for i in 0..n {
            let (mut cur, mut steps) = (i, 0);
            while raw[cur].parent_index >= 0 && (raw[cur].parent_index as usize) < n && !raw[cur].in_parent_ymap && steps <= n {
                cur = raw[cur].parent_index as usize;
                steps += 1;
                if cur == i {
                    h.issues.push(LodIssue { entity: i, kind: LodIssueKind::Cycle, message: "following its LOD parents leads back to itself".into() });
                    break;
                }
            }
        }
        for i in 0..n {
            if raw[i].num_children < h.children[i].len() as u32 {
                h.issues.push(LodIssue { entity: i, kind: LodIssueKind::ChildCountTooLow, message: format!("it says it has {} LOD children but {} entities of this file name it as their parent", raw[i].num_children, h.children[i].len()) });
            }
        }
        h
    }

    /// Check the entities of this map whose LOD parent is in the parent map (`LodInParentYmap`) against `parent`, the map it names.
    pub fn check_against_parent_map(&self, parent: &YmapDocument) -> Vec<LodIssue> {
        let mut out = vec![];
        let praw = parent.raw_lod();
        for i in 0..self.len() {
            let Some(e) = self.entity(i) else { continue };
            if e.flags & LOD_IN_PARENT_YMAP == 0 || e.parent_index < 0 {
                continue;
            }
            let p = e.parent_index as usize;
            if p >= praw.len() {
                out.push(LodIssue { entity: i, kind: LodIssueKind::ParentOutOfRange, message: format!("its LOD parent index {p} is past the last entity of the parent map ({})", praw.len().saturating_sub(1)) });
                continue;
            }
            let (pl, cl) = (praw[p].level, e.lod_level);
            if pl <= cl && pl != LOD_ORPHANHD {
                out.push(LodIssue { entity: i, kind: LodIssueKind::ParentNotHigher, message: format!("its parent (entity {p} of the parent map) is not of a higher LOD level (level {pl} against {cl})") });
            } else if pl == LOD_ORPHANHD && cl != LOD_ORPHANHD {
                out.push(LodIssue { entity: i, kind: LodIssueKind::OrphanParentMismatch, message: format!("its parent (entity {p} of the parent map) is an orphan HD entity") });
            }
        }
        out
    }

    /// Whether `parent` may become the LOD parent of `child`, with the reason when not.
    pub fn can_parent(&self, child: usize, parent: usize) -> Result<(), String> {
        let raw = self.raw_lod();
        if child >= raw.len() || parent >= raw.len() {
            return Err("no such entity".into());
        }
        if let Some((_, why)) = Self::relation_problem(&raw, child, parent) {
            return Err(why);
        }
        // the parent must not be below the child already (that would make a loop)
        let h = self.hierarchy();
        if h.descendants(child).contains(&parent) {
            return Err(format!("entity {parent} is already below entity {child} in the hierarchy: that would make a loop"));
        }
        Ok(())
    }

    fn path(i: usize, member: &str) -> Vec<crate::archetype::edit::Step> {
        vec![field("entities"), Step::Index(i), field(member)]
    }

    /// Make `parent` the LOD parent of `child` (`None`: make it a root). The counts of children of the old and new parent follow, the
    /// "parent is in the parent map" flag is cleared, and it is all one undo step. Refused when the relationship would not be valid.
    pub fn set_parent(&mut self, child: usize, parent: Option<usize>) -> Result<()> {
        self.set_parents_labeled(&[(child, parent)], if parent.is_some() { "Set LOD parent" } else { "Remove LOD parent" })
    }

    /// [`YmapDocument::set_parent`] for several entities as one undo step, checked together (a later pair sees the earlier ones), in one
    /// pass over the file. Nothing changes when any pair is refused.
    pub fn set_parents(&mut self, list: &[(usize, Option<usize>)]) -> Result<()> {
        self.set_parents_labeled(list, if list.len() == 1 { "Set LOD parent" } else { "Set LOD parents" })
    }

    fn set_parents_labeled(&mut self, list: &[(usize, Option<usize>)], label: &str) -> Result<()> {
        let n = self.len();
        let mut raw = self.raw_lod();
        let mut valid_parent = self.hierarchy().parent;
        let original: Vec<u32> = raw.iter().map(|r| r.num_children).collect();
        let mut counts = original.clone();
        let mut new_parent: std::collections::BTreeMap<usize, i32> = Default::default();
        let mut clear_flag: std::collections::BTreeSet<usize> = Default::default();
        for &(child, parent) in list {
            if child >= n {
                bail!("no entity {child}");
            }
            if let Some(p) = parent {
                if p >= n {
                    bail!("no entity {p}");
                }
                if let Some((_, why)) = Self::relation_problem(&raw, child, p) {
                    bail!("{why}");
                }
                // the parent must not be below the child already (that would make a loop)
                let (mut cur, mut steps) = (Some(p), 0);
                while let Some(c) = cur {
                    if c == child {
                        bail!("entity {p} is already below entity {child} in the hierarchy: that would make a loop");
                    }
                    steps += 1;
                    if steps > n {
                        break;
                    }
                    cur = valid_parent[c];
                }
            }
            let old = valid_parent[child];
            if old != parent {
                if let Some(op) = old {
                    counts[op] = counts[op].saturating_sub(1);
                }
                if let Some(np) = parent {
                    counts[np] += 1;
                }
            }
            let new_index = parent.map_or(-1, |p| p as i32);
            // nothing to write when the entity already says exactly this
            if raw[child].parent_index != new_index || raw[child].in_parent_ymap || new_parent.contains_key(&child) {
                if raw[child].in_parent_ymap {
                    clear_flag.insert(child);
                }
                raw[child].parent_index = new_index;
                raw[child].in_parent_ymap = false;
                new_parent.insert(child, new_index);
            }
            valid_parent[child] = parent;
        }
        let mut cmds = vec![];
        for (&child, &idx) in &new_parent {
            cmds.push(Command::Set(Self::path(child, "parentIndex"), MetaValue::I32(idx)));
        }
        for &child in &clear_flag {
            let flags = self.entity(child).map_or(0, |e| e.flags);
            cmds.push(Command::Set(Self::path(child, "flags"), MetaValue::U32(flags & !LOD_IN_PARENT_YMAP)));
        }
        for i in (0..n).filter(|i| counts[*i] != original[*i]) {
            cmds.push(Command::Set(Self::path(i, "numChildren"), MetaValue::U32(counts[i])));
        }
        if cmds.is_empty() {
            return Ok(());
        }
        self.run(Command::Batch(cmds), label, None)
    }

    /// Set the LOD level of several entities as one undo step, checked together: the result has to be valid, so a whole chain can be moved up
    /// a level at once although one entity alone could not. Nothing changes when the result would not be valid.
    pub fn set_lod_levels(&mut self, list: &[(usize, u8)]) -> Result<()> {
        let n = self.len();
        let mut raw = self.raw_lod();
        let h = self.hierarchy();
        for &(i, level) in list {
            if i >= n {
                bail!("no entity {i}");
            }
            if level > 6 {
                bail!("a LOD level is 0 to 6");
            }
            raw[i].level = level;
        }
        let mut cmds = vec![];
        for &(i, level) in list {
            if let Some(p) = h.parent[i] {
                if let Some((_, why)) = Self::relation_problem(&raw, i, p) {
                    bail!("entity {i}: {why}; change its parent first");
                }
            }
            for &c in &h.children[i] {
                if let Some((_, why)) = Self::relation_problem(&raw, c, i) {
                    bail!("entity {i}: {why}; change entity {c} first");
                }
            }
            cmds.push(Command::Set(Self::path(i, "lodLevel"), self.lod_level_value(i, level)));
        }
        if cmds.is_empty() {
            return Ok(());
        }
        self.run(Command::Batch(cmds), "Set LOD levels", None)
    }

    /// the `lodLevel` value for `level`, keeping the member's enum identity
    fn lod_level_value(&self, i: usize, level: u8) -> MetaValue {
        match self.entities()[i].as_struct().and_then(|s| s.field("lodLevel")) {
            // the member name is resolved when the file is written
            Some(MetaValue::Enum { enum_hash, .. }) => MetaValue::Enum { enum_hash: *enum_hash, value: level as i32, name: None },
            _ => MetaValue::I32(level as i32),
        }
    }

    /// Make the stored hierarchy agree with the rules: every `parentIndex` that is not a valid LOD relationship (past the end, itself, not of
    /// a higher level, an orphan HD mismatch, or part of a loop) is removed so the entity is a root, and every `numChildren` that is
    /// lower than the children of this file is raised (with [`CountMode::Exact`] set to exactly them). A parent in the parent map is left
    /// alone. One undo step; nothing is written when there is nothing to do.
    pub fn rebuild_hierarchy(&mut self, mode: CountMode) -> Result<RebuildReport> {
        let n = self.len();
        let mut raw = self.raw_lod();
        let h = self.hierarchy();
        let mut detach: std::collections::BTreeSet<usize> = h
            .issues
            .iter()
            .filter(|i| matches!(i.kind, LodIssueKind::ParentOutOfRange | LodIssueKind::ParentIsItself | LodIssueKind::ParentNotHigher | LodIssueKind::OrphanParentMismatch))
            .map(|i| i.entity)
            .collect();
        for &i in &detach {
            raw[i].parent_index = -1;
        }
        // what is left can still loop (orphan HD entities may be each other's parents)
        let link = |raw: &[Raw], i: usize| -> Option<usize> { (!raw[i].in_parent_ymap && raw[i].parent_index >= 0 && (raw[i].parent_index as usize) < n).then(|| raw[i].parent_index as usize) };
        let mut on_loop = vec![];
        for i in 0..n {
            let (mut cur, mut steps) = (i, 0);
            while let Some(p) = link(&raw, cur) {
                steps += 1;
                if p == i {
                    on_loop.push(i);
                    break;
                }
                if steps > n {
                    break;
                }
                cur = p;
            }
        }
        for i in on_loop {
            raw[i].parent_index = -1;
            detach.insert(i);
        }
        let mut kids = vec![0u32; n];
        for i in 0..n {
            if let Some(p) = link(&raw, i) {
                kids[p] += 1;
            }
        }
        let mut report = RebuildReport::default();
        let mut cmds = vec![];
        for &i in &detach {
            cmds.push(Command::Set(Self::path(i, "parentIndex"), MetaValue::I32(-1)));
            report.detached.push(i);
        }
        for i in 0..n {
            let old = raw[i].num_children;
            let new = match mode {
                CountMode::RaiseOnly => old.max(kids[i]),
                CountMode::Exact => kids[i],
            };
            if new != old {
                cmds.push(Command::Set(Self::path(i, "numChildren"), MetaValue::U32(new)));
                report.counts_changed.push(i);
            }
        }
        if !cmds.is_empty() {
            self.run(Command::Batch(cmds), "Rebuild LOD hierarchy", None)?;
        }
        Ok(report)
    }

    /// Change an entity's LOD level (0..=6). Refused when it would break the relationship with its parent or its children.
    pub fn set_lod_level(&mut self, i: usize, level: u8) -> Result<()> {
        if i >= self.len() {
            bail!("no entity {i}");
        }
        if level > 6 {
            bail!("a LOD level is 0 to 6");
        }
        let mut raw = self.raw_lod();
        let h = self.hierarchy();
        raw[i].level = level;
        if let Some(p) = h.parent[i] {
            if let Some((_, why)) = Self::relation_problem(&raw, i, p) {
                bail!("{why}; change its parent first");
            }
        }
        for &c in &h.children[i] {
            if let Some((_, why)) = Self::relation_problem(&raw, c, i) {
                bail!("{why}; change entity {c} first");
            }
        }
        let value = self.lod_level_value(i, level);
        self.run(Command::Set(Self::path(i, "lodLevel"), value), "Set LOD level", Some(format!("lodlevel{i}")))
    }

    pub fn set_lod_dist(&mut self, i: usize, dist: f32) -> Result<()> {
        if i >= self.len() {
            bail!("no entity {i}");
        }
        if !dist.is_finite() {
            bail!("a LOD distance is a number");
        }
        self.run(Command::Set(Self::path(i, "lodDist"), MetaValue::F32(dist)), "Set LOD distance", Some(format!("loddist{i}")))
    }

    pub fn set_child_lod_dist(&mut self, i: usize, dist: f32) -> Result<()> {
        if i >= self.len() {
            bail!("no entity {i}");
        }
        if !dist.is_finite() {
            bail!("a LOD distance is a number");
        }
        self.run(Command::Set(Self::path(i, "childLodDist"), MetaValue::F32(dist)), "Set child LOD distance", Some(format!("childlod{i}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::schema_view;
    use rage_formats::{build_meta, rage_joaat, Schema};

    /// entities: (level name, parentIndex, numChildren, flags)
    fn doc(list: &[(&str, i32, u32, u32)]) -> YmapDocument {
        let items: String = list
            .iter()
            .enumerate()
            .map(|(i, (lv, parent, kids, flags))| format!(r#"<Item type="CEntityDef"><archetypeName>e{i}</archetypeName><flags value="{flags}"/><guid value="{i}"/><position x="{i}" y="0" z="0"/><rotation x="0" y="0" z="0" w="1"/><scaleXY value="1"/><scaleZ value="1"/><parentIndex value="{parent}"/><lodDist value="100"/><childLodDist value="0"/><lodLevel>{lv}</lodLevel><numChildren value="{kids}"/><priorityLevel>PRI_REQUIRED</priorityLevel><extensions/><ambientOcclusionMultiplier value="255"/><artificialAmbientOcclusion value="255"/><tintValue value="0"/></Item>"#))
            .collect();
        let xml = format!(r#"<CMapData><name>t</name><parent/><flags value="0"/><contentFlags value="0"/><streamingExtentsMin x="0" y="0" z="0"/><streamingExtentsMax x="0" y="0" z="0"/><entitiesExtentsMin x="0" y="0" z="0"/><entitiesExtentsMax x="0" y="0" z="0"/><entities>{items}</entities></CMapData>"#);
        let mut root = rage_formats::from_xml(&xml).unwrap();
        schema_view::coerce_tree(&mut root, rage_joaat("CMapData"));
        YmapDocument::open(&build_meta(&root, Schema::builtin()).unwrap().bytes, None).unwrap()
    }

    const HD: &str = "LODTYPES_DEPTH_HD";
    const LOD: &str = "LODTYPES_DEPTH_LOD";
    const SLOD1: &str = "LODTYPES_DEPTH_SLOD1";
    const ORPHAN: &str = "LODTYPES_DEPTH_ORPHANHD";

    #[test]
    fn a_tree_of_three_levels_has_one_root_and_the_right_children() {
        let d = doc(&[(SLOD1, -1, 1, 0), (LOD, 0, 2, 0), (HD, 1, 0, 0), (HD, 1, 0, 0)]);
        let h = d.hierarchy();
        assert_eq!(h.roots, vec![0]);
        assert_eq!(h.parent, vec![None, Some(0), Some(1), Some(1)]);
        assert_eq!(h.children[1], vec![2, 3]);
        assert_eq!(h.ancestors(3), vec![1, 0]);
        assert_eq!(h.descendants(0), vec![1, 2, 3]);
        assert!(h.issues.is_empty(), "{:?}", h.issues);
    }

    #[test]
    fn a_parent_index_alone_is_not_a_relationship() {
        // 0: HD says its parent is 1 (also HD): same level; 2 names entity 9; 3 names itself; 4 names an orphan HD
        let d = doc(&[(HD, 1, 0, 0), (HD, -1, 1, 0), (HD, 9, 0, 0), (HD, 3, 0, 0), (HD, 5, 0, 0), (ORPHAN, -1, 1, 0)]);
        let h = d.hierarchy();
        assert_eq!(h.roots, vec![0, 1, 2, 3, 4, 5], "none of those are children");
        let kinds: Vec<LodIssueKind> = h.issues.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&LodIssueKind::ParentNotHigher));
        assert!(kinds.contains(&LodIssueKind::ParentOutOfRange));
        assert!(kinds.contains(&LodIssueKind::ParentIsItself));
        assert!(kinds.contains(&LodIssueKind::OrphanParentMismatch));
        assert!(h.issues.iter().all(|i| i.is_error()));
    }

    #[test]
    fn a_parent_in_the_parent_map_is_a_root_here_and_a_note() {
        let d = doc(&[(HD, 4, 0, 8)]);
        let h = d.hierarchy();
        assert_eq!(h.roots, vec![0]);
        assert_eq!(h.issues.len(), 1);
        assert!(!h.issues[0].is_error());
        // and it is checked against the parent map: entity 4 does not exist there
        let parent = doc(&[(LOD, -1, 1, 0)]);
        assert_eq!(d.check_against_parent_map(&parent)[0].kind, LodIssueKind::ParentOutOfRange);
        let parent = doc(&[(LOD, -1, 1, 0), (LOD, -1, 1, 0), (LOD, -1, 1, 0), (LOD, -1, 1, 0), (HD, -1, 1, 0)]);
        assert_eq!(d.check_against_parent_map(&parent)[0].kind, LodIssueKind::ParentNotHigher);
        let parent = doc(&[(LOD, -1, 1, 0), (LOD, -1, 1, 0), (LOD, -1, 1, 0), (LOD, -1, 1, 0), (LOD, -1, 1, 0)]);
        assert!(d.check_against_parent_map(&parent).is_empty());
    }

    #[test]
    fn assigning_and_removing_a_parent_keeps_the_child_counts() {
        let mut d = doc(&[(LOD, -1, 0, 0), (LOD, -1, 0, 0), (HD, -1, 0, 0)]);
        d.set_parent(2, Some(0)).unwrap();
        assert_eq!((d.entity(2).unwrap().parent_index, d.entity(0).unwrap().num_children), (0, 1));
        // moving it to the other parent moves the count too
        d.set_parent(2, Some(1)).unwrap();
        assert_eq!((d.entity(0).unwrap().num_children, d.entity(1).unwrap().num_children), (0, 1));
        d.set_parent(2, None).unwrap();
        assert_eq!((d.entity(2).unwrap().parent_index, d.entity(1).unwrap().num_children), (-1, 0));
        assert!(d.hierarchy().issues.is_empty());
        // all of that is undoable
        d.undo().unwrap();
        assert_eq!((d.entity(2).unwrap().parent_index, d.entity(1).unwrap().num_children), (1, 1));
    }

    #[test]
    fn invalid_assignments_are_refused() {
        let mut d = doc(&[(SLOD1, -1, 1, 0), (LOD, 0, 1, 0), (HD, 1, 0, 0), (ORPHAN, -1, 0, 0)]);
        assert!(d.set_parent(0, Some(0)).unwrap_err().to_string().contains("its own"));
        assert!(d.set_parent(0, Some(1)).is_err(), "a higher level cannot sit under a lower one");
        assert!(d.set_parent(2, Some(3)).unwrap_err().to_string().contains("orphan"));
        assert!(d.set_parent(2, Some(2)).is_err());
        assert!(d.set_parent(2, Some(9)).is_err());
        // a loop: 1 under 2 would put 2 (its descendant) above it; the level rule already refuses, can_parent says why
        assert!(d.can_parent(1, 2).is_err());
        // nothing changed
        assert_eq!(d.hierarchy().parent, vec![None, Some(0), Some(1), None]);
    }

    #[test]
    fn changing_a_level_must_keep_the_relations_valid() {
        let mut d = doc(&[(SLOD1, -1, 1, 0), (LOD, 0, 1, 0), (HD, 1, 0, 0)]);
        assert!(d.set_lod_level(1, 0).is_err(), "LOD to HD: it would no longer be above its child");
        assert!(d.set_lod_level(1, 2).is_err(), "LOD to SLOD1: it would no longer be below its parent");
        assert!(d.set_lod_level(1, 7).is_err());
        d.set_lod_level(2, 0).unwrap();
        // an HD child can also be made orphan HD only if its parent allows (an ordinary parent does not mind)
        assert_eq!(d.entity(1).unwrap().lod_level, 1);
        let lone = &mut doc(&[(HD, -1, 0, 0)]);
        lone.set_lod_level(0, 3).unwrap();
        assert_eq!(lone.entity(0).unwrap().lod_level, 3);
        lone.set_lod_dist(0, 250.0).unwrap();
        lone.set_child_lod_dist(0, 80.0).unwrap();
        let e = lone.entity(0).unwrap();
        assert_eq!((e.lod_dist, e.child_lod_dist), (250.0, 80.0));
    }

    #[test]
    fn deleting_a_child_updates_its_parent_and_deleting_a_parent_is_refused() {
        let mut d = doc(&[(LOD, -1, 1, 0), (HD, 0, 0, 0), (HD, -1, 0, 0)]);
        assert!(d.remove_entity(0).is_err(), "it still has a child");
        d.remove_entity(1).unwrap();
        assert_eq!(d.entity(0).unwrap().num_children, 0);
        d.remove_entity(0).unwrap();
        assert_eq!(d.len(), 1);
        assert!(d.hierarchy().issues.is_empty());
    }

    #[test]
    fn a_loop_in_the_raw_indices_is_reported() {
        let d = doc(&[(HD, 1, 0, 0), (HD, 0, 0, 0)]);
        let h = d.hierarchy();
        assert!(h.issues.iter().any(|i| i.kind == LodIssueKind::Cycle), "{:?}", h.issues);
        assert_eq!(h.roots, vec![0, 1]);
    }

    #[test]
    fn several_parents_are_set_in_one_step_and_checked_together() {
        let mut d = doc(&[(LOD, -1, 0, 0), (LOD, -1, 0, 0), (HD, -1, 0, 0), (HD, -1, 0, 0), (HD, -1, 0, 0)]);
        let steps = d.undo_steps();
        d.set_parents(&[(2, Some(0)), (3, Some(0)), (4, Some(1))]).unwrap();
        assert_eq!(d.undo_steps(), steps + 1);
        assert_eq!((d.entity(0).unwrap().num_children, d.entity(1).unwrap().num_children), (2, 1));
        assert_eq!(d.hierarchy().parent, vec![None, None, Some(0), Some(0), Some(1)]);
        // moving one of them again inside a batch counts the children once
        d.set_parents(&[(2, Some(1)), (3, None)]).unwrap();
        assert_eq!((d.entity(0).unwrap().num_children, d.entity(1).unwrap().num_children), (0, 2));
        // a later pair sees the earlier one: 0 under 1 is fine (SLOD1 above LOD would be, LOD under LOD is not)
        assert!(d.set_parents(&[(2, Some(0)), (4, Some(9))]).is_err());
        assert_eq!(d.hierarchy().parent, vec![None, None, Some(1), None, Some(1)], "a refused batch changes nothing");
        d.undo().unwrap();
        assert_eq!(d.hierarchy().parent, vec![None, None, Some(0), Some(0), Some(1)]);
        // an entity whose parent was in the parent map loses that flag when it gets a parent here
        let mut p = doc(&[(LOD, -1, 0, 0), (HD, 4, 0, 8)]);
        p.set_parents(&[(1, Some(0))]).unwrap();
        assert_eq!(p.entity(1).unwrap().flags & 8, 0);
        assert_eq!(p.entity(1).unwrap().parent_index, 0);
    }

    #[test]
    fn setting_what_is_already_there_writes_nothing() {
        let mut d = doc(&[(LOD, -1, 1, 0), (HD, 0, 0, 0), (HD, -1, 0, 0)]);
        let steps = d.undo_steps();
        d.set_parents(&[(1, Some(0)), (2, None)]).unwrap();
        assert_eq!(d.undo_steps(), steps, "both already say that");
    }

    #[test]
    fn levels_are_set_together_so_a_whole_chain_can_move_up_in_any_order() {
        let mut d = doc(&[(SLOD1, -1, 1, 0), (LOD, 0, 1, 0), (HD, 1, 0, 0)]);
        // one at a time, the child first is refused: it cannot reach LOD while its parent is still LOD
        assert!(d.set_lod_level(2, 1).is_err());
        let steps = d.undo_steps();
        // together the result is what counts: SLOD1 -> SLOD2, LOD -> SLOD1, HD -> LOD
        d.set_lod_levels(&[(2, 1), (1, 2), (0, 3)]).unwrap();
        assert_eq!(d.undo_steps(), steps + 1);
        assert_eq!((d.entity(0).unwrap().lod_level, d.entity(1).unwrap().lod_level, d.entity(2).unwrap().lod_level), (3, 2, 1));
        assert!(d.hierarchy().issues.is_empty());
        // a result that breaks a relation is refused whole
        assert!(d.set_lod_levels(&[(0, 0), (2, 1)]).is_err());
        assert!(d.set_lod_levels(&[(0, 9)]).is_err());
        assert!(d.set_lod_levels(&[(7, 1)]).is_err());
        assert_eq!(d.entity(0).unwrap().lod_level, 3);
        d.undo().unwrap();
        assert_eq!(d.entity(0).unwrap().lod_level, 2);
    }

    #[test]
    fn rebuilding_removes_what_is_not_a_relationship_and_raises_low_counts() {
        // 0 names itself, 1 names a missing entity, 2 is HD under HD, 3 is a good child of 4 (whose count says 0), 5 and 6 are orphan HD
        // entities that are each other's parent (a loop the level rule allows), 7 names a parent in the parent map
        let mut d = doc(&[(HD, 0, 0, 0), (HD, 9, 0, 0), (HD, 0, 0, 0), (HD, 4, 0, 0), (LOD, -1, 0, 0), (ORPHAN, 6, 0, 0), (ORPHAN, 5, 0, 0), (HD, 2, 0, 8)]);
        assert!(d.hierarchy().issues.iter().any(|i| i.is_error()));
        let steps = d.undo_steps();
        let r = d.rebuild_hierarchy(CountMode::RaiseOnly).unwrap();
        assert_eq!(d.undo_steps(), steps + 1);
        assert_eq!(r.detached, vec![0, 1, 2, 5, 6]);
        assert_eq!(r.counts_changed, vec![4]);
        assert!(d.hierarchy().issues.iter().all(|i| !i.is_error()), "{:?}", d.hierarchy().issues);
        assert_eq!(d.entity(3).unwrap().parent_index, 4, "a valid relation is kept");
        assert_eq!(d.entity(4).unwrap().num_children, 1);
        assert_eq!(d.entity(7).unwrap().parent_index, 2, "a parent in the parent map is left alone");
        // nothing more to do: no step, nothing reported
        let again = d.rebuild_hierarchy(CountMode::RaiseOnly).unwrap();
        assert_eq!((again, d.undo_steps()), (RebuildReport::default(), steps + 1));
        d.undo().unwrap();
        assert_eq!(d.entity(1).unwrap().parent_index, 9);
    }

    #[test]
    fn exact_counts_drop_children_that_live_in_other_files_and_raise_only_keeps_them() {
        let mut d = doc(&[(LOD, -1, 7, 0), (HD, 0, 0, 0)]);
        let keep = d.rebuild_hierarchy(CountMode::RaiseOnly).unwrap();
        assert!(keep.counts_changed.is_empty() && d.entity(0).unwrap().num_children == 7);
        let exact = d.rebuild_hierarchy(CountMode::Exact).unwrap();
        assert_eq!(exact.counts_changed, vec![0]);
        assert_eq!(d.entity(0).unwrap().num_children, 1);
    }

    #[test]
    fn a_saved_and_reopened_hierarchy_is_the_same() {
        let mut d = doc(&[(LOD, -1, 0, 0), (HD, -1, 0, 0), (HD, -1, 0, 0)]);
        d.set_parent(1, Some(0)).unwrap();
        d.set_parent(2, Some(0)).unwrap();
        let back = YmapDocument::open(&d.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.hierarchy().parent, d.hierarchy().parent);
        assert_eq!(back.entity(0).unwrap().num_children, 2);
    }
}
