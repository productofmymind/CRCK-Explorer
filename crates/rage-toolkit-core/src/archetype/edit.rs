//! Editing a `MetaValue` tree by path, with undo and redo. A [`Path`] names one value (`archetypes[3].lodDist`); a [`Command`] changes
//! the tree and hands back the command that undoes it, which is all [`History`] needs. No GUI types here.
use anyhow::{bail, Context, Result};
use rage_formats::{rage_joaat, MetaValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// the member of a structure with this name hash
    Field(u32),
    /// the n-th item of an array
    Index(usize),
}

pub type Path = Vec<Step>;

/// `path!["archetypes", 3, "lodDist"]`-style construction from names and indices
pub fn field(name: &str) -> Step {
    Step::Field(rage_joaat(name))
}

pub fn get<'a>(root: &'a MetaValue, path: &[Step]) -> Option<&'a MetaValue> {
    let mut v = root;
    for s in path {
        v = match (s, v) {
            (Step::Field(h), MetaValue::Struct(st)) => st.get(*h)?,
            (Step::Index(i), MetaValue::Array(a)) => a.items.get(*i)?,
            _ => return None,
        };
    }
    Some(v)
}

pub fn get_mut<'a>(root: &'a mut MetaValue, path: &[Step]) -> Option<&'a mut MetaValue> {
    let mut v = root;
    for s in path {
        v = match (s, v) {
            (Step::Field(h), MetaValue::Struct(st)) => st.fields.iter_mut().find(|(k, _)| k == h).map(|(_, v)| v)?,
            (Step::Index(i), MetaValue::Array(a)) => a.items.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(v)
}

/// One change to the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// replace the value at the path (a member that is not there yet is added)
    Set(Path, MetaValue),
    /// insert an item into the array at the path, before `index`
    Insert(Path, usize, MetaValue),
    /// remove the item `index` of the array at the path
    Remove(Path, usize),
    /// take a member out of the structure at the path
    RemoveField(Path, u32),
    /// several changes done one after the other that undo as one (an import of many archetypes)
    Batch(Vec<Command>),
}

/// Do `cmd`; the result undoes it.
pub fn apply(root: &mut MetaValue, cmd: Command) -> Result<Command> {
    match cmd {
        Command::Set(path, value) => {
            if path.is_empty() {
                let old = std::mem::replace(root, value);
                return Ok(Command::Set(path, old));
            }
            let (parent, last) = path.split_at(path.len() - 1);
            let parent_v = get_mut(root, parent).context("the parent of that value is not there")?;
            match (last[0], parent_v) {
                (Step::Field(h), MetaValue::Struct(st)) => {
                    if let Some((_, slot)) = st.fields.iter_mut().find(|(k, _)| *k == h) {
                        let old = std::mem::replace(slot, value);
                        Ok(Command::Set(path, old))
                    } else {
                        st.fields.push((h, value));
                        Ok(Command::RemoveField(parent.to_vec(), h))
                    }
                }
                (Step::Index(i), MetaValue::Array(a)) => {
                    let slot = a.items.get_mut(i).context("no such item")?;
                    let old = std::mem::replace(slot, value);
                    Ok(Command::Set(path, old))
                }
                _ => bail!("that value cannot be set here"),
            }
        }
        Command::Insert(path, index, value) => {
            let MetaValue::Array(a) = get_mut(root, &path).context("no such list")? else { bail!("that is not a list") };
            if index > a.items.len() {
                bail!("the position is past the end of the list");
            }
            a.items.insert(index, value);
            Ok(Command::Remove(path, index))
        }
        Command::Remove(path, index) => {
            let MetaValue::Array(a) = get_mut(root, &path).context("no such list")? else { bail!("that is not a list") };
            if index >= a.items.len() {
                bail!("no such item");
            }
            let old = a.items.remove(index);
            Ok(Command::Insert(path, index, old))
        }
        Command::Batch(cmds) => {
            let mut undo: Vec<Command> = Vec::with_capacity(cmds.len());
            for c in cmds {
                match apply(root, c) {
                    Ok(u) => undo.push(u),
                    Err(e) => {
                        // put back what was done: a batch is all or nothing
                        for u in undo.into_iter().rev() {
                            let _ = apply(root, u);
                        }
                        return Err(e);
                    }
                }
            }
            undo.reverse();
            Ok(Command::Batch(undo))
        }
        Command::RemoveField(path, h) => {
            let MetaValue::Struct(st) = get_mut(root, &path).context("no such structure")? else { bail!("that is not a structure") };
            let i = st.fields.iter().position(|(k, _)| *k == h).context("no such member")?;
            let (_, old) = st.fields.remove(i);
            let mut p = path;
            p.push(Step::Field(h));
            Ok(Command::Set(p, old))
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    /// what the user did, for a menu ("Edit lodDist", "Remove archetype")
    label: String,
    undo: Command,
    redo: Command,
    /// consecutive edits with the same key are one undo step (dragging a number, typing in a field)
    key: Option<String>,
}

#[derive(Debug)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
    /// how many entries of `done` the file on disk has (None: it has a state that is no longer reachable)
    saved_at: Option<usize>,
}

impl Default for History {
    fn default() -> Self {
        // a freshly opened file matches its tree
        Self { done: vec![], undone: vec![], saved_at: Some(0) }
    }
}

impl History {
    /// Apply and record. `key`: edits with the same key right after one another merge into one undo step.
    pub fn run(&mut self, root: &mut MetaValue, cmd: Command, label: &str, key: Option<String>) -> Result<()> {
        let undo = apply(root, cmd.clone())?;
        self.undone.clear();
        if self.saved_at.map(|s| s > self.done.len()).unwrap_or(false) {
            self.saved_at = None;
        }
        let at_saved = self.saved_at == Some(self.done.len());
        if let (Some(k), Some(last)) = (&key, self.done.last_mut()) {
            if last.key.as_ref() == Some(k) && !at_saved {
                // keep the first undo, take the newest redo
                last.redo = cmd;
                return Ok(());
            }
        }
        self.done.push(Entry { label: label.to_string(), undo, redo: cmd, key });
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.done.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.undone.last().map(|e| e.label.as_str())
    }

    pub fn undo(&mut self, root: &mut MetaValue) -> Result<()> {
        let Some(mut e) = self.done.pop() else { bail!("nothing to undo") };
        let redo = apply(root, e.undo.clone())?;
        e.redo = redo;
        e.key = None;
        self.undone.push(e);
        Ok(())
    }

    pub fn redo(&mut self, root: &mut MetaValue) -> Result<()> {
        let Some(mut e) = self.undone.pop() else { bail!("nothing to redo") };
        let undo = apply(root, e.redo.clone())?;
        e.undo = undo;
        self.done.push(e);
        Ok(())
    }

    /// the file now matches the tree
    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.done.len());
        for e in &mut self.done {
            e.key = None;
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_at != Some(self.done.len())
    }

    pub fn steps(&self) -> usize {
        self.done.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rage_formats::value::{MetaArray, MetaStruct};

    fn tree() -> MetaValue {
        let item = |n: i32| MetaValue::Struct(MetaStruct { type_hash: 1, fields: vec![(rage_joaat("a"), MetaValue::I32(n))] });
        MetaValue::Struct(MetaStruct { type_hash: 0, fields: vec![(rage_joaat("list"), MetaValue::Array(MetaArray { item_type: None, typed_items: false, items: vec![item(1), item(2)] }))] })
    }

    fn a_of(t: &MetaValue, i: usize) -> Option<i64> {
        get(t, &[field("list"), Step::Index(i), field("a")]).and_then(MetaValue::as_i64)
    }

    #[test]
    fn set_insert_remove_and_their_undos_restore_the_tree() {
        let original = tree();
        let mut t = original.clone();
        let mut h = History::default();
        let p = vec![field("list"), Step::Index(1), field("a")];
        h.run(&mut t, Command::Set(p.clone(), MetaValue::I32(9)), "edit", None).unwrap();
        assert_eq!(a_of(&t, 1), Some(9));
        h.run(&mut t, Command::Insert(vec![field("list")], 0, MetaValue::Struct(MetaStruct { type_hash: 1, fields: vec![(rage_joaat("a"), MetaValue::I32(5))] })), "add", None).unwrap();
        assert_eq!((a_of(&t, 0), a_of(&t, 2)), (Some(5), Some(9)));
        h.run(&mut t, Command::Remove(vec![field("list")], 1), "remove", None).unwrap();
        assert_eq!((a_of(&t, 0), a_of(&t, 1)), (Some(5), Some(9)));
        // a member that is not there is added, and undo takes it out again
        h.run(&mut t, Command::Set(vec![field("extra")], MetaValue::I32(3)), "add field", None).unwrap();
        assert!(get(&t, &[field("extra")]).is_some());
        h.undo(&mut t).unwrap();
        assert!(get(&t, &[field("extra")]).is_none());
        // undo everything, then redo everything
        while h.can_undo() {
            h.undo(&mut t).unwrap();
        }
        assert_eq!(t, original);
        while h.can_redo() {
            h.redo(&mut t).unwrap();
        }
        assert_eq!((a_of(&t, 0), a_of(&t, 1)), (Some(5), Some(9)));
        // a new edit forgets the redo list
        h.undo(&mut t).unwrap();
        h.run(&mut t, Command::Set(p, MetaValue::I32(1)), "again", None).unwrap();
        assert!(!h.can_redo());
        // bad paths are errors, not panics
        assert!(h.run(&mut t, Command::Remove(vec![field("list")], 99), "x", None).is_err());
        assert!(h.run(&mut t, Command::Set(vec![field("nope"), Step::Index(0)], MetaValue::Null), "x", None).is_err());
    }

    #[test]
    fn typing_in_one_field_is_one_undo_step_and_the_saved_marker_follows_it() {
        let mut t = tree();
        let mut h = History::default();
        let p = vec![field("list"), Step::Index(0), field("a")];
        for v in [10, 11, 12] {
            h.run(&mut t, Command::Set(p.clone(), MetaValue::I32(v)), "edit a", Some("a0".into())).unwrap();
        }
        assert_eq!(h.steps(), 1);
        assert!(h.is_dirty());
        h.mark_saved();
        assert!(!h.is_dirty());
        // an edit right after saving is a new step even with the same key (the saved state must stay reachable)
        h.run(&mut t, Command::Set(p.clone(), MetaValue::I32(13)), "edit a", Some("a0".into())).unwrap();
        assert_eq!(h.steps(), 2);
        assert!(h.is_dirty());
        h.undo(&mut t).unwrap();
        assert_eq!(a_of(&t, 0), Some(12));
        assert!(!h.is_dirty(), "back at the saved state");
        h.undo(&mut t).unwrap();
        assert_eq!(a_of(&t, 0), Some(1), "one undo undoes the whole typing");
        assert!(h.is_dirty());
    }
}
