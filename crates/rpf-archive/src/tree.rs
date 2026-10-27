use crate::archive::{RpfEntry, RpfEntryKind};

/// A node in an RPF directory tree. Stores indices into the flat `RpfArchive.entries` slice.
#[derive(Debug, Clone)]
pub struct DirNode {
    pub name  : String,
    pub path  : String,
    pub files : Vec<FileRef>,
    pub subdirs: Vec<DirNode>,
}

/// A reference to a file entry within a `DirNode`, keyed by entry index.
#[derive(Debug, Clone)]
pub struct FileRef {
    pub name        : String,
    pub path        : String,
    pub entry_index : usize,   // index into RpfArchive.entries
    pub size        : u32,     // on-disk / compressed size
    pub mem_size    : u32,     // uncompressed (binary) or resource page size (resource)
    pub is_resource : bool,
}

/// Build a navigable directory tree from a flat `RpfArchive.entries` slice.
pub fn build_directory_tree(entries: &[RpfEntry]) -> DirNode {
    if entries.is_empty() {
        return DirNode { name: String::new(), path: String::new(), files: vec![], subdirs: vec![] };
    }

    // If the first entry is a directory, it is the root.
    let (root_name, start, count) = if let RpfEntryKind::Directory { entries_index, entries_count } = &entries[0].kind {
        (entries[0].name.clone(), *entries_index as usize, *entries_count as usize)
    } else {
        // Flat archive (no directory wrapper) — put everything in a synthetic root.
        return flat_root(entries);
    };

    let mut root = DirNode { name: root_name, path: String::new(), files: vec![], subdirs: vec![] };
    // CRCK Explorer patch: directory ranges come from the file. In a sound archive every entry but the root is listed once, so
    // `entries.len()` visits are enough; a damaged or crafted one (a folder listing itself or its parent) used to recurse
    // until the stack overflowed and the program aborted.
    let mut budget = entries.len();
    populate(&mut root, entries, start, count, "", 0, &mut budget);
    root
}

/// deeper than any real archive (the game's go about 10 levels down)
const MAX_TREE_DEPTH: usize = 128;

/// Returns a flat list of all `FileRef`s in the tree (breadth-first).
pub fn list_all_files(node: &DirNode) -> Vec<&FileRef> {
    let mut out = Vec::new();
    collect(node, &mut out);
    out
}

// ─── Internals ────────────────────────────────────────────────────────────────

fn populate(dir: &mut DirNode, entries: &[RpfEntry], start: usize, count: usize, parent_path: &str, depth: usize, budget: &mut usize) {
    let end = start.saturating_add(count).min(entries.len());
    for i in start..end {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let entry = &entries[i];
        match &entry.kind {
            RpfEntryKind::Directory { entries_index, entries_count } => {
                let sub_path = child_path(parent_path, &entry.name_lower);
                let mut sub = DirNode { name: entry.name.clone(), path: sub_path.clone(), files: vec![], subdirs: vec![] };
                if depth < MAX_TREE_DEPTH {
                    populate(&mut sub, entries, *entries_index as usize, *entries_count as usize, &sub_path, depth + 1, budget);
                }
                dir.subdirs.push(sub);
            }
            RpfEntryKind::BinaryFile { file_size, uncompressed_size, .. } => {
                let path = child_path(parent_path, &entry.name_lower);
                dir.files.push(FileRef {
                    name: entry.name.clone(), path, entry_index: i,
                    size: *file_size, mem_size: *uncompressed_size, is_resource: false,
                });
            }
            RpfEntryKind::ResourceFile { file_size, system_flags, .. } => {
                let path = child_path(parent_path, &entry.name_lower);
                let mem_size = crate::archive::resource_size_from_flags(*system_flags) as u32;
                dir.files.push(FileRef {
                    name: entry.name.clone(), path, entry_index: i,
                    size: *file_size, mem_size, is_resource: true,
                });
            }
        }
    }
}

fn flat_root(entries: &[RpfEntry]) -> DirNode {
    let mut root = DirNode { name: String::new(), path: String::new(), files: vec![], subdirs: vec![] };
    for (i, entry) in entries.iter().enumerate() {
        match &entry.kind {
            RpfEntryKind::BinaryFile { file_size, uncompressed_size, .. } => {
                root.files.push(FileRef {
                    name: entry.name.clone(), path: entry.name_lower.clone(), entry_index: i,
                    size: *file_size, mem_size: *uncompressed_size, is_resource: false,
                });
            }
            RpfEntryKind::ResourceFile { file_size, system_flags, .. } => {
                let mem_size = crate::archive::resource_size_from_flags(*system_flags) as u32;
                root.files.push(FileRef {
                    name: entry.name.clone(), path: entry.name_lower.clone(), entry_index: i,
                    size: *file_size, mem_size, is_resource: true,
                });
            }
            _ => {}
        }
    }
    root
}

fn collect<'a>(node: &'a DirNode, out: &mut Vec<&'a FileRef>) {
    out.extend(node.files.iter());
    for sub in &node.subdirs { collect(sub, out); }
}

fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{}/{}", parent, name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str, entries_index: u32, entries_count: u32) -> RpfEntry {
        RpfEntry { name: name.into(), name_lower: name.into(), kind: RpfEntryKind::Directory { entries_index, entries_count } }
    }

    fn file(name: &str) -> RpfEntry {
        RpfEntry { name: name.into(), name_lower: name.into(), kind: RpfEntryKind::BinaryFile { file_offset: 0, file_size: 0, uncompressed_size: 1, is_encrypted: false } }
    }

    #[test]
    fn a_sound_tree_is_built_in_full() {
        let entries = vec![dir("", 1, 2), dir("a", 3, 2), file("x.txt"), file("y.ytd"), file("z.ydr")];
        let root = build_directory_tree(&entries);
        assert_eq!(root.files.len(), 1);
        assert_eq!(root.subdirs[0].files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a/y.ytd", "a/z.ydr"]);
    }

    #[test]
    fn a_folder_that_lists_itself_does_not_overflow_the_stack() {
        // entry 1 is a folder whose children are entries 0..2: the root and itself
        let entries = vec![dir("", 1, 2), dir("loop", 0, 3), file("x.txt")];
        let root = build_directory_tree(&entries);
        assert!(list_all_files(&root).len() <= entries.len());
    }
}
