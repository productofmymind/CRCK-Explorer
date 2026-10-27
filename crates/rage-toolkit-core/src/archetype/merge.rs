//! Merging another `.ytyp` into the open one, the way the game's structures work (not by pasting XML together): the two files' archetype
//! lists are compared archetype by archetype by name hash. An archetype only the other file has is **new**; one both have with the same
//! content is **identical**; one both have with different content is a **conflict** the user decides. Nothing changes until the plan is
//! applied, and the whole merge is one undo step. The merged file can be checked (`fork`, then `validate`) before it is saved.
use super::{
    document::{archetype_path, ArchKind, YtypDocument},
    edit::{field, Command},
    fidelity::compare_trees,
    schema_view,
};
use anyhow::{bail, Result};
use rage_formats::{rage_joaat, MetaValue};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// only the other file has it
    New,
    /// both have it, with the same content
    Identical,
    /// both have it, with different content (the members that differ are listed)
    Conflict,
    /// the other file has two archetypes of this name; this is the second
    RepeatedInOther,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// index in the other file
    pub index: usize,
    pub name: String,
    pub name_hash: u32,
    pub kind: ArchKind,
    pub status: Status,
    /// for a conflict: the members that differ, e.g. `lodDist`, `bbMax`
    pub differing: Vec<String>,
    /// index of the archetype of the same name in the open file
    pub existing: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// add it to the open file
    Add,
    /// leave it out
    Skip,
    /// overwrite the open file's archetype of the same name
    Replace,
    /// add it under another name (the original stays)
    AddAs(String),
}

#[derive(Debug, Clone, Default)]
pub struct MergePlan {
    pub entries: Vec<Entry>,
    /// items of the other file's own `extensions` / `compositeEntityTypes` lists that the open file lacks
    pub extra_items: usize,
}

impl MergePlan {
    pub fn count(&self, s: Status) -> usize {
        self.entries.iter().filter(|e| e.status == s).count()
    }
}

/// What to do unless told otherwise: add the new ones, leave everything else.
pub fn default_choices(plan: &MergePlan) -> Vec<Choice> {
    plan.entries.iter().map(|e| if e.status == Status::New { Choice::Add } else { Choice::Skip }).collect()
}

fn differing_members(a: &MetaValue, b: &MetaValue, doc: &YtypDocument) -> Vec<String> {
    let (diffs, _) = compare_trees(a, b, &doc.names);
    let (back, _) = compare_trees(b, a, &doc.names);
    let mut out: Vec<String> = vec![];
    for d in diffs.iter().chain(back.iter()) {
        // ".lodDist" or ".extensions[0].name": the first member of the path
        let first = d.path.trim_start_matches('.').split(['.', '[']).next().unwrap_or("").to_string();
        if !first.is_empty() && !out.contains(&first) {
            out.push(first);
        }
    }
    out
}

/// Compare the two files. Changes nothing.
pub fn plan(base: &YtypDocument, other: &YtypDocument) -> MergePlan {
    let mut mine: HashMap<u32, usize> = HashMap::new();
    for i in 0..base.len() {
        mine.entry(base.name_hash(i)).or_insert(i);
    }
    let mut seen_in_other: HashMap<u32, usize> = HashMap::new();
    let mut entries = vec![];
    for (i, a) in other.archetypes().iter().enumerate() {
        let h = other.name_hash(i);
        let name = other.name(i);
        let kind = ArchKind::of(a);
        let (status, differing, existing) = if seen_in_other.contains_key(&h) && h != 0 {
            (Status::RepeatedInOther, vec![], mine.get(&h).copied())
        } else {
            seen_in_other.insert(h, i);
            match mine.get(&h) {
                None => (Status::New, vec![], None),
                Some(&j) => {
                    let theirs = &base.archetypes()[j];
                    let (d1, n1) = compare_trees(theirs, a, &base.names);
                    let (_, n2) = compare_trees(a, theirs, &base.names);
                    let _ = d1;
                    if n1 == 0 && n2 == 0 {
                        (Status::Identical, vec![], Some(j))
                    } else {
                        (Status::Conflict, differing_members(theirs, a, base), Some(j))
                    }
                }
            }
        };
        entries.push(Entry { index: i, name, name_hash: h, kind, status, differing, existing });
    }
    // the file's own top-level lists
    let mut extra = 0;
    for list in ["extensions", "compositeEntityTypes"] {
        let (mine, theirs) = (base.get(&[field(list)]).map(|v| v.items()).unwrap_or(&[]), other.get(&[field(list)]).map(|v| v.items()).unwrap_or(&[]));
        extra += theirs.iter().filter(|t| !mine.iter().any(|m| compare_trees(m, t, &base.names).1 == 0)).count();
    }
    MergePlan { entries, extra_items: extra }
}

fn renamed(item: &MetaValue, old: u32, new_name: &str) -> MetaValue {
    let new = schema_view::name_hash(new_name);
    let mut copy = item.clone();
    if let MetaValue::Struct(s) = &mut copy {
        for (h, v) in s.fields.iter_mut() {
            if (*h == rage_joaat("name") || *h == rage_joaat("assetName")) && v.as_hash() == Some(old) {
                *v = MetaValue::Hash(new);
            }
        }
    }
    copy
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub added: usize,
    pub replaced: usize,
    pub renamed: usize,
    pub skipped: usize,
    pub extra_items: usize,
}

/// Do the merge as one undo step. `choices` has one entry per `plan.entries`. A rename that collides with a name in the open file (or
/// with another rename) is an error and nothing changes.
pub fn apply(base: &mut YtypDocument, other: &YtypDocument, plan: &MergePlan, choices: &[Choice], with_extra_items: bool) -> Result<MergeReport> {
    if choices.len() != plan.entries.len() {
        bail!("one choice per archetype is needed");
    }
    let mut report = MergeReport::default();
    let mut cmds: Vec<Command> = vec![];
    let mut next = base.len();
    let mut taken: std::collections::HashSet<u32> = (0..base.len()).map(|i| base.name_hash(i)).collect();
    for (e, c) in plan.entries.iter().zip(choices) {
        let item = other.archetypes()[e.index].clone();
        match c {
            Choice::Skip => report.skipped += 1,
            Choice::Add => {
                if !taken.insert(e.name_hash) && e.name_hash != 0 {
                    bail!("{} is already in the file: choose Replace, Add as another name, or Skip", e.name);
                }
                cmds.push(Command::Insert(vec![field("archetypes")], next, item));
                next += 1;
                report.added += 1;
            }
            Choice::Replace => {
                let Some(j) = e.existing else { bail!("{} has nothing to replace", e.name) };
                cmds.push(Command::Set(archetype_path(j), item));
                report.replaced += 1;
            }
            Choice::AddAs(n) => {
                let h = schema_view::name_hash(n);
                if n.trim().is_empty() || !taken.insert(h) {
                    bail!("the new name {n:?} for {} is empty or already used", e.name);
                }
                base.names.add_lowercase(n.trim());
                cmds.push(Command::Insert(vec![field("archetypes")], next, renamed(&item, e.name_hash, n)));
                next += 1;
                report.renamed += 1;
            }
        }
    }
    if with_extra_items {
        for list in ["extensions", "compositeEntityTypes"] {
            let theirs = other.get(&[field(list)]).map(|v| v.items().to_vec()).unwrap_or_default();
            let mine: Vec<MetaValue> = base.get(&[field(list)]).map(|v| v.items().to_vec()).unwrap_or_default();
            let mut at = mine.len();
            let exists = base.get(&[field(list)]).is_some();
            let fresh: Vec<MetaValue> = theirs.into_iter().filter(|t| !mine.iter().any(|m| compare_trees(m, t, &base.names).1 == 0)).collect();
            if fresh.is_empty() {
                continue;
            }
            if !exists {
                cmds.push(Command::Set(vec![field(list)], MetaValue::Array(Default::default())));
            }
            for t in fresh {
                cmds.push(Command::Insert(vec![field(list)], at, t));
                at += 1;
                report.extra_items += 1;
            }
        }
    }
    if cmds.is_empty() {
        return Ok(report);
    }
    // learn the other file's names so the merged archetypes show as text
    for e in &plan.entries {
        if e.name_hash != 0 && !e.name.starts_with("hash_") {
            base.names.add_lowercase(&e.name);
        }
    }
    base.run(Command::Batch(cmds), "Merge archetypes", None)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::validate::validate;

    fn doc(items: &str) -> YtypDocument {
        let xml = format!("<CMapTypes><extensions/><archetypes>{items}</archetypes><name>t</name></CMapTypes>");
        let mut d = YtypDocument::open(xml.as_bytes(), None).unwrap();
        for n in ["prop_one", "prop_two", "prop_three", "prop_four", "prop_two_b", "prop_other"] {
            d.names.add_lowercase(n);
        }
        d
    }

    fn base_item(name: &str, lod: u32) -> String {
        format!("<Item type=\"CBaseArchetypeDef\"><lodDist value=\"{lod}\"/><bbMin x=\"-1\" y=\"-1\" z=\"-1\"/><bbMax x=\"1\" y=\"1\" z=\"1\"/><bsRadius value=\"2\"/><name>{name}</name><assetName>{name}</assetName></Item>")
    }

    #[test]
    fn the_plan_tells_new_from_identical_from_conflicting_by_the_games_structure() {
        let base = doc(&(base_item("prop_one", 100) + &base_item("prop_two", 100)));
        let other = doc(&(base_item("prop_one", 100) + &base_item("PROP_TWO", 250) + &base_item("prop_three", 100) + &base_item("prop_three", 100)));
        let p = plan(&base, &other);
        let st: Vec<(&str, Status)> = p.entries.iter().map(|e| (e.name.as_str(), e.status.clone())).collect();
        assert_eq!(st.len(), 4);
        assert_eq!(p.entries[0].status, Status::Identical);
        assert_eq!(p.entries[1].status, Status::Conflict, "same name in another case is the same archetype");
        assert_eq!(p.entries[1].differing, ["lodDist"]);
        assert_eq!(p.entries[1].existing, Some(1));
        assert_eq!(p.entries[2].status, Status::New);
        assert_eq!(p.entries[3].status, Status::RepeatedInOther);
        assert_eq!(default_choices(&p), [Choice::Skip, Choice::Skip, Choice::Add, Choice::Skip]);
        assert_eq!(base.len(), 2, "planning changes nothing");
    }

    #[test]
    fn applying_adds_replaces_renames_and_skips_as_one_undo_step() {
        let mut base = doc(&(base_item("prop_one", 100) + &base_item("prop_two", 100)));
        let before = base.to_xml(None);
        let other = doc(&(base_item("prop_two", 250) + &base_item("prop_three", 100) + &base_item("prop_four", 100)));
        let p = plan(&base, &other);
        let r = apply(&mut base, &other, &p, &[Choice::Replace, Choice::Add, Choice::AddAs("prop_other".into())], true).unwrap();
        assert_eq!(r, MergeReport { added: 1, replaced: 1, renamed: 1, skipped: 0, extra_items: 0 });
        assert_eq!(base.len(), 4);
        assert_eq!(base.get(&[field("archetypes"), crate::archetype::edit::Step::Index(1), field("lodDist")]).and_then(|v| v.as_f32()), Some(250.0), "replaced");
        assert_eq!(base.name(2), "prop_three");
        assert_eq!(base.name(3), "prop_other");
        assert_eq!(base.get(&[field("archetypes"), crate::archetype::edit::Step::Index(3), field("assetName")]).and_then(|v| v.as_hash()), Some(rage_joaat("prop_other")), "assetName followed the new name");
        assert!(validate(&base, None).iter().all(|i| i.code != "ARCH_DUPLICATE"));
        // one undo takes the whole merge back
        base.undo().unwrap();
        assert_eq!(base.to_xml(None), before);
        assert!(!base.can_undo());
        base.redo().unwrap();
        assert_eq!(base.len(), 4);
    }

    #[test]
    fn a_name_clash_is_refused_and_changes_nothing() {
        let mut base = doc(&base_item("prop_one", 100));
        let other = doc(&(base_item("prop_one", 5) + &base_item("prop_two", 100)));
        let p = plan(&base, &other);
        assert!(apply(&mut base, &other, &p, &[Choice::Add, Choice::Add], true).is_err(), "adding a second prop_one");
        assert!(apply(&mut base, &other, &p, &[Choice::Skip, Choice::AddAs("prop_one".into())], true).is_err(), "renaming onto a name in use");
        assert!(apply(&mut base, &other, &p, &[Choice::Skip], true).is_err(), "a choice for each");
        assert_eq!(base.len(), 1);
        assert!(!base.can_undo());
    }

    #[test]
    fn the_merged_file_can_be_checked_before_it_is_saved_and_saves() {
        let base = doc(&base_item("prop_one", 100));
        let other = doc(&"<Item type=\"CBaseArchetypeDef\"><lodDist value=\"-4\"/><name>prop_two</name></Item>".to_string());
        let p = plan(&base, &other);
        // the merge is tried on a copy and the copy is checked
        let mut trial = base.fork();
        apply(&mut trial, &other, &p, &default_choices(&p), true).unwrap();
        assert!(validate(&trial, None).iter().any(|i| i.code == "ARCH_LOD"), "the problem shows before anything is applied");
        assert_eq!(base.len(), 1);
        // a clean one merges and saves
        let other2 = doc(&base_item("prop_two", 100));
        let p2 = plan(&base, &other2);
        let mut real = base.fork();
        apply(&mut real, &other2, &p2, &default_choices(&p2), true).unwrap();
        let bytes = real.to_bytes().unwrap();
        assert_eq!(YtypDocument::open(&bytes, None).unwrap().len(), 2);
    }

    #[test]
    fn mlo_archetypes_and_the_files_own_extension_lists_merge_too() {
        let mlo = "<Item type=\"CMloArchetypeDef\"><lodDist value=\"60\"/><name>prop_one</name><rooms><Item><name>limbo</name></Item></rooms><portals/><entities/></Item>";
        let mut base = doc("");
        // a file-level extension in the other file only
        let ext = "<Item type=\"CExtensionDefParticleEffect\"><name>fx</name></Item>";
        let mut other = YtypDocument::open(format!("<CMapTypes><extensions>{ext}</extensions><archetypes>{mlo}</archetypes><name>t</name></CMapTypes>").as_bytes(), None).unwrap();
        other.names.add_lowercase("prop_one");
        let p = plan(&base, &other);
        assert_eq!((p.entries[0].kind, p.extra_items), (ArchKind::Mlo, 1));
        let r = apply(&mut base, &other, &p, &default_choices(&p), true).unwrap();
        assert_eq!((r.added, r.extra_items), (1, 1));
        assert_eq!(base.kind(0), ArchKind::Mlo);
        assert_eq!(base.get(&[field("extensions")]).map(|v| v.items().len()), Some(1));
        base.to_bytes().unwrap();
    }
}
