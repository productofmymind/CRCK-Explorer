//! Editing an RPF7 archive in place, the way the classic tools' Edit mode does: add, replace, delete and rename files, also inside
//! archives nested in it (`x64e.rpf` > `levels/gta5/vehicles.rpf`).
//!
//! - **Append only.** New data goes at the end of the file; only the tables of contents (at the start of each archive) are
//!   written over, innermost archive first. Nothing an old table points at is overwritten, except the few files a growing
//!   table needs the room of (those are copied to the end first). A multi-GB archive is never rebuilt.
//! - **Nested archives grow to the end of the file:** their size in the parent's table is raised so the appended data is
//!   inside them (no copy of a 1.9 GB inner archive).
//! - **Saved unencrypted** (OPEN, the classic tools' format; the game reads it with the mods-folder ASI plugin and an ASI loader). The entries of an
//!   edited archive that were encrypted are decrypted into new copies once, so every reader sees plain data.
//! - **Checked:** after writing, the archives are read back and every edit is compared; any difference is an error.
//! - **Never the game's own files:** pass the game and mods folders and those archives are refused (`crate::mods`).
//!
//! Layout facts used here, measured on all 5,045 archives of a retail install: name tables stay under 64 KB (16-bit name
//! offsets), a folder's children are sorted by lowercase name, the root is entry 0 and lists from entry 1, data is in
//! 512-byte blocks, resources over 16 MB keep their size in their own stored header.
use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use rpf_archive::{
    crypto::{decrypt_aes, decrypt_ng, keys::GtaKeys},
    RpfArchive, RpfEncryption, RpfEntry, RpfEntryKind,
};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

const RPF7: u32 = 0x52504637;
const RSC7: u32 = 0x37435352;
const BLOCK: u64 = 512;
/// block offsets are 23 bits (bit 23 marks a resource): 4 GB from the archive's start
const MAX_BLOCKS: u64 = 0x7FFFFF;
/// a 24-bit size field holds less than this; bigger resources keep their size in their stored header
const BIG: u32 = 0xFFFFFF;

/// One change to an archive. Paths are inside the archive being edited, `/` or `\` separated, any case.
#[derive(Clone, Debug)]
pub enum Edit {
    /// add the file, or replace it when one of that name is there; missing folders are made
    Put { path: String, data: Vec<u8> },
    /// a file, or a folder with everything in it
    Delete { path: String },
    /// another name for a file or folder, in the same folder
    Rename { path: String, to: String },
}

#[derive(Clone, Debug)]
enum Item {
    Dir { name: String, kids: Vec<Item> },
    File { name: String, data: Data },
}

impl Item {
    fn name(&self) -> &str {
        match self {
            Item::Dir { name, .. } | Item::File { name, .. } => name,
        }
    }
}

/// Where a file's bytes are. Offsets are 512-byte blocks from the start of the archive the entry belongs to.
#[derive(Clone, Debug)]
enum Data {
    /// `file_size` 0 = stored as is (`size` bytes), else deflated to `file_size` bytes
    Binary { offset: u32, file_size: u32, size: u32, encrypted: bool },
    /// `file_size` = the stored length including its 16-byte header (the real size, also when over 16 MB)
    Resource { offset: u32, file_size: u32, sys: u32, gfx: u32 },
    /// to be written at the end of the file; becomes `entry` with its offset filled in
    New { source: Source, entry: Box<Data> },
}

#[derive(Clone, Debug)]
enum Source {
    Bytes(Vec<u8>),
    /// copy `len` bytes from this absolute position of the file as it was (a file moved out of a growing table's way)
    Copy { from: u64, len: u64 },
}

fn stored_len(d: &Data) -> u64 {
    match d {
        Data::Binary { file_size, size, .. } => (if *file_size > 0 { *file_size } else { *size }) as u64,
        Data::Resource { file_size, .. } => *file_size as u64,
        Data::New { source: Source::Bytes(b), .. } => b.len() as u64,
        Data::New { source: Source::Copy { len, .. }, .. } => *len,
    }
}

fn offset_of(d: &Data) -> Option<u32> {
    match d {
        Data::Binary { offset, .. } | Data::Resource { offset, .. } => Some(*offset),
        Data::New { .. } => None,
    }
}

/// One archive on the way from the file to the one edited.
struct Level {
    /// absolute position in the file (a multiple of 512)
    start: u64,
    name: String,
    enc: RpfEncryption,
    root: Item,
    /// for a nested archive: its path in the parent
    path_in_parent: Option<String>,
}

fn parts(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|p| !p.is_empty()).collect()
}

/// A name the archive can hold and every tool can extract: no separators, drive or stream marks, not `.` or `..`.
fn check_name(n: &str) -> Result<()> {
    if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\\', ':', '\0']) {
        bail!("{n:?} is not a name a file in an archive can have");
    }
    Ok(())
}

fn tree(entries: &[RpfEntry]) -> Item {
    fn item(entries: &[RpfEntry], i: usize, budget: &mut usize, depth: usize) -> Item {
        let e = &entries[i];
        match &e.kind {
            RpfEntryKind::Directory { entries_index, entries_count } => {
                let mut kids = vec![];
                if depth < 128 {
                    let (s, c) = (*entries_index as usize, *entries_count as usize);
                    for k in s.min(entries.len())..s.saturating_add(c).min(entries.len()) {
                        if *budget == 0 {
                            break;
                        }
                        *budget -= 1;
                        kids.push(item(entries, k, budget, depth + 1));
                    }
                }
                Item::Dir { name: e.name.clone(), kids }
            }
            RpfEntryKind::BinaryFile { file_offset, file_size, uncompressed_size, is_encrypted } => Item::File { name: e.name.clone(), data: Data::Binary { offset: *file_offset, file_size: *file_size, size: *uncompressed_size, encrypted: *is_encrypted } },
            RpfEntryKind::ResourceFile { file_offset, file_size, system_flags, graphics_flags, .. } => Item::File { name: e.name.clone(), data: Data::Resource { offset: *file_offset, file_size: *file_size, sys: *system_flags, gfx: *graphics_flags } },
        }
    }
    match entries.first().map(|e| &e.kind) {
        Some(RpfEntryKind::Directory { .. }) => item(entries, 0, &mut entries.len(), 0),
        _ => Item::Dir { name: String::new(), kids: (0..entries.len()).filter(|&i| !entries[i].is_directory()).map(|i| item(entries, i, &mut 1, 0)).collect() },
    }
}

/// the folder at `path` (made when `make`), as a mutable list of its items
fn dir_mut<'a>(root: &'a mut Item, path: &[&str], make: bool) -> Result<&'a mut Vec<Item>> {
    let Item::Dir { kids, .. } = root else { bail!("not a folder") };
    let mut kids = kids;
    for p in path {
        let low = p.to_lowercase();
        let i = match kids.iter().position(|k| k.name().to_lowercase() == low) {
            Some(i) => i,
            None if make => {
                check_name(p)?;
                kids.push(Item::Dir { name: p.to_string(), kids: vec![] });
                kids.len() - 1
            }
            None => bail!("{p} not found"),
        };
        match &mut kids[i] {
            Item::Dir { kids: k, .. } => kids = k,
            Item::File { name, .. } => bail!("{name} is a file, not a folder"),
        }
    }
    Ok(kids)
}

fn find<'a>(root: &'a Item, path: &[&str]) -> Option<&'a Item> {
    let mut cur = root;
    for p in path {
        let Item::Dir { kids, .. } = cur else { return None };
        let low = p.to_lowercase();
        cur = kids.iter().find(|k| k.name().to_lowercase() == low)?;
    }
    Some(cur)
}

fn find_mut<'a>(root: &'a mut Item, path: &[&str]) -> Option<&'a mut Item> {
    let mut cur = root;
    for p in path {
        let Item::Dir { kids, .. } = cur else { return None };
        let low = p.to_lowercase();
        cur = kids.iter_mut().find(|k| k.name().to_lowercase() == low)?;
    }
    Some(cur)
}

/// The entry a file's bytes become: a resource when they start with an RSC7 header, else a binary file (deflated when that
/// is smaller; an archive inside is stored as is, as the game keeps them).
fn encode(name: &str, data: Vec<u8>) -> Result<Data> {
    if data.is_empty() {
        bail!("{name}: an empty file cannot be stored in an RPF archive");
    }
    if data.len() >= 16 && u32::from_le_bytes(data[0..4].try_into().unwrap()) == RSC7 {
        let (sys, gfx) = (u32::from_le_bytes(data[8..12].try_into().unwrap()), u32::from_le_bytes(data[12..16].try_into().unwrap()));
        let len = u32::try_from(data.len()).context("a resource of 4 GB or more")?;
        let mut stored = data;
        if len >= BIG {
            // the 24-bit size field cannot hold it: the size goes into the stored header, where the game reads it
            stored[7] = len as u8;
            stored[14] = (len >> 8) as u8;
            stored[5] = (len >> 16) as u8;
            stored[2] = (len >> 24) as u8;
        }
        return Ok(Data::New { source: Source::Bytes(stored), entry: Box::new(Data::Resource { offset: 0, file_size: len, sys, gfx }) });
    }
    let size = u32::try_from(data.len()).context("a file of 4 GB or more")?;
    if !name.to_lowercase().ends_with(".rpf") {
        use flate2::{write::DeflateEncoder, Compression};
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(&data)?;
        let packed = e.finish()?;
        if packed.len() < data.len() && (packed.len() as u64) < BIG as u64 {
            let file_size = packed.len() as u32;
            return Ok(Data::New { source: Source::Bytes(packed), entry: Box::new(Data::Binary { offset: 0, file_size, size, encrypted: false }) });
        }
    }
    Ok(Data::New { source: Source::Bytes(data), entry: Box::new(Data::Binary { offset: 0, file_size: 0, size, encrypted: false }) })
}

fn apply(root: &mut Item, edit: &Edit) -> Result<()> {
    match edit {
        Edit::Put { path, data } => {
            let p = parts(path);
            let (name, dir) = p.split_last().context("no file name")?;
            check_name(name)?;
            let entry = encode(name, data.clone())?;
            let kids = dir_mut(root, dir, true)?;
            let low = name.to_lowercase();
            match kids.iter_mut().find(|k| k.name().to_lowercase() == low) {
                Some(Item::File { data, .. }) => *data = entry,
                Some(Item::Dir { .. }) => bail!("{path} is a folder"),
                None => kids.push(Item::File { name: name.to_string(), data: entry }),
            }
        }
        Edit::Delete { path } => {
            let p = parts(path);
            let (name, dir) = p.split_last().context("nothing to delete")?;
            let kids = dir_mut(root, dir, false).with_context(|| format!("deleting {path}"))?;
            let low = name.to_lowercase();
            let before = kids.len();
            kids.retain(|k| k.name().to_lowercase() != low);
            if kids.len() == before {
                bail!("{path} not found");
            }
        }
        Edit::Rename { path, to } => {
            check_name(to)?;
            let p = parts(path);
            let (name, dir) = p.split_last().context("nothing to rename")?;
            let kids = dir_mut(root, dir, false).with_context(|| format!("renaming {path}"))?;
            let (low, to_low) = (name.to_lowercase(), to.to_lowercase());
            if to_low != low && kids.iter().any(|k| k.name().to_lowercase() == to_low) {
                bail!("{to} is there already");
            }
            match kids.iter_mut().find(|k| k.name().to_lowercase() == low) {
                Some(Item::Dir { name, .. } | Item::File { name, .. }) => *name = to.clone(),
                None => bail!("{path} not found"),
            }
        }
    }
    Ok(())
}

/// The table of contents for `root`: header, entries (each folder's children together, sorted by lowercase name) and the
/// names, as an OPEN (unencrypted) RPF7. Every file must have its place by now (no `Data::New`).
fn write_toc(root: &Item) -> Result<Vec<u8>> {
    let mut flat: Vec<&Item> = vec![root];
    let mut ranges: Vec<(u32, u32)> = vec![(0, 0)];
    let mut i = 0;
    while i < flat.len() {
        if let Item::Dir { kids, .. } = flat[i] {
            let mut k: Vec<&Item> = kids.iter().collect();
            k.sort_by_key(|a| a.name().to_lowercase());
            ranges[i] = (flat.len() as u32, k.len() as u32);
            for x in k {
                flat.push(x);
                ranges.push((0, 0));
            }
        }
        i += 1;
    }
    let mut names: Vec<u8> = vec![0]; // the root's empty name at 0
    let mut entries = Vec::with_capacity(flat.len() * 16);
    for (i, item) in flat.iter().enumerate() {
        let name_off = if i == 0 {
            0
        } else {
            let o = names.len();
            names.extend_from_slice(item.name().as_bytes());
            names.push(0);
            o
        };
        if name_off > 0xFFFF {
            bail!("too many names for one archive: the name table is limited to 64 KB");
        }
        match item {
            Item::Dir { .. } => {
                entries.extend_from_slice(&(name_off as u32).to_le_bytes());
                entries.extend_from_slice(&0x7FFFFF00u32.to_le_bytes());
                entries.extend_from_slice(&ranges[i].0.to_le_bytes());
                entries.extend_from_slice(&ranges[i].1.to_le_bytes());
            }
            Item::File { data, name } => {
                let (fsize, off, a, b, resource) = match data {
                    Data::Binary { offset, file_size, size, .. } => (*file_size, *offset, *size, 0u32, false),
                    Data::Resource { offset, file_size, sys, gfx } => ((*file_size).min(BIG), *offset, *sys, *gfx, true),
                    Data::New { .. } => bail!("{name} has no place in the file yet"),
                };
                if fsize > BIG || off as u64 >= MAX_BLOCKS {
                    bail!("{name}: size or position out of range for an RPF7 entry");
                }
                let off = if resource { off | 0x800000 } else { off };
                entries.extend_from_slice(&(name_off as u16).to_le_bytes());
                entries.extend_from_slice(&fsize.to_le_bytes()[..3]);
                entries.extend_from_slice(&off.to_le_bytes()[..3]);
                entries.extend_from_slice(&a.to_le_bytes());
                entries.extend_from_slice(&b.to_le_bytes());
            }
        }
    }
    names.resize(names.len().next_multiple_of(16), 0);
    let mut out = Vec::with_capacity(16 + entries.len() + names.len());
    out.extend_from_slice(&RPF7.to_le_bytes());
    out.extend_from_slice(&(flat.len() as u32).to_le_bytes());
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    out.extend_from_slice(&RpfEncryption::Open.as_u32().to_le_bytes());
    out.extend_from_slice(&entries);
    out.extend_from_slice(&names);
    Ok(out)
}

/// Every file of the tree with its path; the first error stops the walk.
fn visit(item: &mut Item, path: &mut Vec<String>, f: &mut dyn FnMut(&[String], &mut Data) -> Result<()>) -> Result<()> {
    match item {
        Item::Dir { kids, .. } => {
            for k in kids.iter_mut() {
                path.push(k.name().to_string());
                visit(k, path, f)?;
                path.pop();
            }
            Ok(())
        }
        Item::File { data, .. } => f(path, data),
    }
}

/// Decrypt `stored` the way the reader does: a binary file whole (keyed by its name and size), a resource after its header.
fn decrypt(enc: RpfEncryption, keys: &GtaKeys, name: &str, d: &Data, stored: &[u8]) -> Vec<u8> {
    let run = |b: &[u8], len: u32| match enc {
        RpfEncryption::Aes => decrypt_aes(b, &keys.aes_key),
        _ => decrypt_ng(b, keys, name, len),
    };
    match d {
        Data::Binary { size, .. } => run(stored, *size),
        Data::Resource { file_size, .. } => {
            let mut out = stored[..16.min(stored.len())].to_vec();
            out.extend(run(&stored[16.min(stored.len())..], *file_size));
            out
        }
        Data::New { .. } => stored.to_vec(),
    }
}

/// What [`edit`] changed, for the user.
#[derive(Debug, Default)]
pub struct Report {
    pub put: usize,
    pub deleted: usize,
    pub renamed: usize,
    /// archives that were encrypted and are now OPEN
    pub converted: Vec<String>,
    /// bytes added at the end of the file
    pub appended: u64,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut s = vec![];
        if self.put > 0 {
            s.push(format!("{} file(s) written", self.put));
        }
        if self.deleted > 0 {
            s.push(format!("{} deleted", self.deleted));
        }
        if self.renamed > 0 {
            s.push(format!("{} renamed", self.renamed));
        }
        if !self.converted.is_empty() {
            s.push(format!("{} saved unencrypted (OPEN)", self.converted.join(", ")));
        }
        s.push(format!("{:.1} MB added to the file", self.appended as f64 / 1048576.0));
        s.join("; ")
    }
}

/// Apply `edits` to the archive `chain` leads to inside the archive file `disk` (`chain` empty = the file itself; else the
/// paths of the nested archives on the way, each relative to the one before, e.g. `["levels/gta5/vehicles.rpf"]`).
/// `keys` are needed when an archive on the way is still encrypted (a fresh copy of a game archive). `game` = the game and
/// mods folders: an archive that is one of the game's own files is refused.
pub fn edit(disk: &Path, chain: &[String], edits: &[Edit], keys: Option<&GtaKeys>, game: Option<(&Path, &Path)>) -> Result<Report> {
    if let Some((g, m)) = game {
        if crate::mods::game_relative(g, m, disk).is_some() {
            bail!("{} is one of the game's own archives: CRCK Explorer never writes into those. Copy it to the mods folder and edit the copy.", disk.display());
        }
    } else if let Some(g) = disk.ancestors().skip(1).find(|a| a.join("GTA5.exe").is_file()) {
        // no folders given, but this is inside a game folder: only its mods folder may be written
        if crate::mods::game_relative(g, &crate::mods::default_dir(g), disk).is_some() {
            bail!("{} is inside the game folder and not in its mods folder: refused", disk.display());
        }
    }
    if edits.is_empty() {
        bail!("nothing to do");
    }
    let mut file = fs::OpenOptions::new().read(true).write(true).open(disk).with_context(|| format!("opening {} for writing (is the game or another tool using it?)", disk.display()))?;
    // SAFETY: read-only view of the file as it is now; this function only appends to the file and rewrites table areas it
    // has already read, and other readers of the file see consistent old data (nothing they point at is overwritten).
    let map = unsafe { Mmap::map(&file)? };
    let file_name = disk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    // the archives on the way
    let mut levels: Vec<Level> = vec![];
    let parse = |start: u64, size: u64, name: &str| -> Result<(RpfEncryption, Item)> {
        let end = start.checked_add(size).filter(|e| *e <= map.len() as u64).with_context(|| format!("{name} extends past the end of the file"))?;
        let ar = RpfArchive::parse_at(&map[..end as usize], start as usize, name, keys).with_context(|| format!("reading {name}"))?;
        if ar.version != rpf_archive::RpfVersion::V7 {
            bail!("{name}: only GTA V archives (RPF7) can be edited");
        }
        if ar.encryption.is_encrypted() && keys.is_none() {
            bail!("{name} is encrypted: GTA V must be found to read and convert it");
        }
        Ok((ar.encryption, tree(&ar.entries)))
    };
    let (enc, root) = parse(0, map.len() as u64, &file_name)?;
    levels.push(Level { start: 0, name: file_name.clone(), enc, root, path_in_parent: None });
    for inner in chain {
        let parent = levels.last().unwrap();
        let p = parts(inner);
        let Some(Item::File { name, data }) = find(&parent.root, &p) else { bail!("{inner} not found in {}", parent.name) };
        let Data::Binary { offset, file_size: 0, size, .. } = data else { bail!("{inner} is not an archive stored as is") };
        let (start, size, name) = (parent.start + *offset as u64 * BLOCK, *size as u64, name.clone());
        let (enc, root) = parse(start, size, &name)?;
        levels.push(Level { start, name, enc, root, path_in_parent: Some(inner.clone()) });
    }

    let mut report = Report::default();
    // every archive on the way gets a new table, so encrypted ones become OPEN: their encrypted files are decrypted into new
    // copies. Before the edits: the key of a file depends on its name, which a rename changes.
    for lv in levels.iter_mut() {
        if !lv.enc.is_encrypted() {
            continue;
        }
        let keys = keys.context("an encrypted archive needs the game's keys")?;
        let (enc, start, label) = (lv.enc, lv.start, lv.name.clone());
        visit(&mut lv.root, &mut vec![], &mut |path, d| {
            let name = path.last().map(String::as_str).unwrap_or("");
            let encrypted = match d {
                Data::Binary { encrypted, .. } => *encrypted,
                Data::Resource { .. } => name.to_lowercase().ends_with(".ysc"),
                Data::New { .. } => false,
            };
            if !encrypted {
                return Ok(());
            }
            let from = start + offset_of(d).unwrap_or(0) as u64 * BLOCK;
            let stored = map.get(from as usize..(from + stored_len(d)) as usize).with_context(|| format!("{label}: {} is out of bounds", path.join("/")))?;
            let plain = decrypt(enc, keys, name, d, stored);
            let mut entry = d.clone();
            if let Data::Binary { encrypted, .. } = &mut entry {
                *encrypted = false;
            }
            *d = Data::New { source: Source::Bytes(plain), entry: Box::new(entry) };
            Ok(())
        })?;
        report.converted.push(lv.name.clone());
    }
    // the edits, on the innermost archive
    {
        let target = levels.last_mut().unwrap();
        for e in edits {
            apply(&mut target.root, e).with_context(|| format!("in {}", target.name))?;
            match e {
                Edit::Put { .. } => report.put += 1,
                Edit::Delete { .. } => report.deleted += 1,
                Edit::Rename { .. } => report.renamed += 1,
            }
        }
    }

    // a table that grows needs the blocks after it: the files there are copied to the end first
    let mut tocs: Vec<usize> = vec![];
    for lv in levels.iter_mut() {
        let len = write_toc_len(&lv.root)?;
        let (blocks, start) = ((len as u64).div_ceil(BLOCK), lv.start);
        visit(&mut lv.root, &mut vec![], &mut |_, d| {
            if let Some(off) = offset_of(d).filter(|o| (*o as u64) < blocks) {
                let len = stored_len(d);
                *d = Data::New { source: Source::Copy { from: start + off as u64 * BLOCK, len }, entry: Box::new(d.clone()) };
            }
            Ok(())
        })?;
        tocs.push(len);
    }

    // place everything new at the end of the file, in 512-byte blocks
    let old_len = map.len() as u64;
    let mut end = old_len.next_multiple_of(BLOCK);
    let mut writes: Vec<(u64, Source)> = vec![];
    for lv in levels.iter_mut() {
        let (start, label) = (lv.start, lv.name.clone());
        visit(&mut lv.root, &mut vec![], &mut |_, d| {
            let Data::New { entry, .. } = d else { return Ok(()) };
            let blocks = (end - start) / BLOCK;
            if blocks >= MAX_BLOCKS {
                bail!("{label} has reached the 4 GB an RPF7 archive can address: copy it to the mods folder again from the game and make the edits there");
            }
            let mut placed = (**entry).clone();
            if let Data::Binary { offset, .. } | Data::Resource { offset, .. } = &mut placed {
                *offset = blocks as u32;
            }
            let len = stored_len(d);
            if let Data::New { source, .. } = std::mem::replace(d, placed) {
                writes.push((end, source));
            }
            end = (end + len).next_multiple_of(BLOCK);
            Ok(())
        })?;
    }
    report.appended = end.saturating_sub(old_len);
    // nested archives reach to the end of the file, so what was appended for them is inside them
    if end > old_len {
        for i in 1..levels.len() {
            let size = u32::try_from(end - levels[i].start).with_context(|| format!("{} would be 4 GB or more", levels[i].name))?;
            let path = levels[i].path_in_parent.clone().unwrap_or_default();
            match find_mut(&mut levels[i - 1].root, &parts(&path)) {
                Some(Item::File { data: Data::Binary { size: s, file_size: 0, .. }, .. }) => *s = size,
                _ => bail!("{path} moved"),
            }
        }
    }

    // write: the data, then the tables from the innermost archive out
    for (pos, source) in &writes {
        file.seek(SeekFrom::Start(*pos))?;
        match source {
            Source::Bytes(b) => file.write_all(b)?,
            Source::Copy { from, len } => {
                let src = map.get(*from as usize..(*from + *len) as usize).context("a file to move is out of bounds")?;
                for chunk in src.chunks(8 << 20) {
                    file.write_all(chunk)?;
                }
            }
        }
    }
    if end > old_len {
        file.set_len(end)?;
    }
    file.sync_data()?;
    let tables: Vec<Vec<u8>> = levels.iter().map(|lv| write_toc(&lv.root)).collect::<Result<_>>()?;
    for (lv, (t, len)) in levels.iter().zip(tables.iter().zip(&tocs)).rev() {
        if t.len() != *len {
            bail!("internal: the table of {} is {} bytes, room was made for {len}", lv.name, t.len());
        }
        file.seek(SeekFrom::Start(lv.start))?;
        file.write_all(t)?;
    }
    file.sync_data()?;
    drop(map);
    drop(file);
    verify(disk, chain, edits).with_context(|| format!("{} was written but does not read back as expected", disk.display()))?;
    Ok(report)
}

/// the length of the table `write_toc` makes, without needing every file placed
fn write_toc_len(root: &Item) -> Result<usize> {
    fn count(i: &Item, n: &mut usize, names: &mut usize) {
        *n += 1;
        *names += i.name().len() + 1;
        if let Item::Dir { kids, .. } = i {
            kids.iter().for_each(|k| count(k, n, names));
        }
    }
    let (mut n, mut names) = (0, 0);
    count(root, &mut n, &mut names);
    // the root's name is the single 0 at the start
    names -= root.name().len();
    if names > 0x10000 {
        bail!("too many names for one archive: the name table is limited to 64 KB");
    }
    Ok(16 + n * 16 + names.next_multiple_of(16))
}

/// Read the archive back (it is OPEN now: no keys) and check every edit.
fn verify(disk: &Path, chain: &[String], edits: &[Edit]) -> Result<()> {
    let f = fs::File::open(disk)?;
    // SAFETY: read-only view for the check; nothing writes the file meanwhile
    let data = unsafe { Mmap::map(&f)? };
    let index = |ar: &RpfArchive| -> std::collections::HashMap<String, usize> { crate::game::list_files(&ar.entries).into_iter().map(|(p, i)| (p.to_lowercase(), i)).collect() };
    let key = |path: &str| parts(path).join("/").to_lowercase();
    let (mut start, mut size) = (0usize, data.len());
    let name = disk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut ar = RpfArchive::parse_at(&data[..size], 0, &name, None)?;
    for inner in chain {
        let i = *index(&ar).get(&key(inner)).with_context(|| format!("{inner} is gone"))?;
        let RpfEntryKind::BinaryFile { file_offset, uncompressed_size, .. } = ar.entries[i].kind else { bail!("{inner} is not an archive") };
        start += file_offset as usize * 512;
        size = uncompressed_size as usize;
        let bounded = data.get(..start + size).context("a nested archive extends past the end")?;
        let inner_name = ar.entries[i].name.clone();
        ar = RpfArchive::parse_at(bounded, start, &inner_name, None)?;
    }
    let bounded = &data[..start + size];
    let files = index(&ar);
    let is_dir = |p: &str| {
        let pre = format!("{}/", key(p));
        files.keys().any(|k| k.starts_with(&pre))
    };
    // the last edit of a path decides what must be there
    let touched_later = |k: usize, path: &str| {
        edits[k + 1..].iter().any(|x| match x {
            Edit::Put { path: p, .. } | Edit::Delete { path: p } | Edit::Rename { path: p, .. } => key(p) == key(path),
        })
    };
    for (k, e) in edits.iter().enumerate() {
        match e {
            Edit::Put { path, data: want } if !touched_later(k, path) => {
                let i = *files.get(&key(path)).with_context(|| format!("{path} is missing"))?;
                let got = ar.extract_entry(bounded, &ar.entries[i], None)?;
                if &got != want {
                    bail!("{path} reads back different ({} bytes instead of {})", got.len(), want.len());
                }
            }
            Edit::Delete { path } if !touched_later(k, path) => {
                if files.contains_key(&key(path)) || is_dir(path) {
                    bail!("{path} is still there");
                }
            }
            Edit::Rename { path, to } if !touched_later(k, path) => {
                let mut p = parts(path);
                p.pop();
                p.push(to);
                let new = p.join("/");
                if !files.contains_key(&key(&new)) && !is_dir(&new) {
                    bail!("{new} is missing after the rename");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Archive;
    use rpf_archive::RpfBuilder;
    use std::{collections::BTreeMap, path::PathBuf};

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_rpfedit_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn build(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut b = RpfBuilder::new(RpfEncryption::Open);
        for (p, d) in files {
            b.add_file(p, d.clone());
        }
        b.build(None).unwrap()
    }

    /// every file of an archive (nested archives listed as files, as `Archive` does), lowercase path -> bytes
    fn contents(ar: &Archive) -> BTreeMap<String, Vec<u8>> {
        ar.files().into_iter().map(|f| (f.path.to_lowercase(), ar.read(&f.path).unwrap())).collect()
    }

    /// a fake RSC7 resource: header (magic, version 13, flags whose top bits say 13) and a body
    fn resource(body_len: usize, fill: u8) -> Vec<u8> {
        let mut d = vec![];
        d.extend_from_slice(&RSC7.to_le_bytes());
        d.extend_from_slice(&13u32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&0xD000_0000u32.to_le_bytes());
        d.extend((0..body_len).map(|i| fill ^ (i as u8)));
        d
    }

    #[test]
    fn edits_inside_a_nested_archive_keep_everything_else() {
        let d = temp("nested");
        let meta = b"<CVehicleModelInfo__InitDataList>".repeat(40);
        let inner = build(&[("a.txt", b"old a".to_vec()), ("sub/b.meta", meta.clone()), ("c.ytd", resource(3000, 7)), ("d.txt", b"stays".to_vec())]);
        let outer = build(&[("x/inner.rpf", inner), ("top.txt", b"top".to_vec())]);
        let path = d.join("dlc.rpf");
        fs::write(&path, &outer).unwrap();
        let edits = vec![
            Edit::Put { path: "sub/new.xml".into(), data: b"<new/>".to_vec() },
            Edit::Put { path: "A.TXT".into(), data: b"new a, longer than before".to_vec() },
            Edit::Delete { path: "c.ytd".into() },
            Edit::Rename { path: "sub/b.meta".into(), to: "b2.meta".into() },
            Edit::Put { path: "new folder/deep/big.ytd".into(), data: resource(20_000, 3) },
        ];
        let r = edit(&path, &["x/inner.rpf".to_string()], &edits, None, None).unwrap();
        assert_eq!((r.put, r.deleted, r.renamed), (3, 1, 1));
        assert!(r.appended < 64 * 1024, "only the new data is added ({} bytes)", r.appended);
        let ar = Archive::open(&path, None).unwrap();
        assert_eq!(ar.read("top.txt").unwrap(), b"top");
        let inner = contents(&ar.child("x/inner.rpf").unwrap());
        let want: BTreeMap<String, Vec<u8>> = [
            ("a.txt", b"new a, longer than before".to_vec()),
            ("d.txt", b"stays".to_vec()),
            ("new folder/deep/big.ytd", resource(20_000, 3)),
            ("sub/b2.meta", meta),
            ("sub/new.xml", b"<new/>".to_vec()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        assert_eq!(inner, want);
        // and again on the edited file: the archives are OPEN now and keep working
        edit(&path, &["x/inner.rpf".to_string()], &[Edit::Delete { path: "new folder".into() }], None, None).unwrap();
        let ar = Archive::open(&path, None).unwrap();
        assert_eq!(contents(&ar.child("x/inner.rpf").unwrap()).len(), 4);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_growing_table_moves_the_files_in_its_way() {
        let d = temp("grow");
        let original: Vec<(String, Vec<u8>)> = (0..8).map(|i| (format!("f{i}.dat"), vec![i as u8 + 1; 700 + i * 13])).collect();
        let path = d.join("small.rpf");
        fs::write(&path, build(&original.iter().map(|(p, v)| (p.as_str(), v.clone())).collect::<Vec<_>>())).unwrap();
        // 60 long names: the table needs several more 512-byte blocks, where the first files are
        let edits: Vec<Edit> = (0..60).map(|i| Edit::Put { path: format!("added/a_rather_long_file_name_to_fill_the_name_table_{i:03}.txt"), data: format!("file {i}").into_bytes() }).collect();
        edit(&path, &[], &edits, None, None).unwrap();
        let got = contents(&Archive::open(&path, None).unwrap());
        assert_eq!(got.len(), 68);
        for (p, v) in &original {
            assert_eq!(&got[p], v, "{p} survives the move");
        }
        assert_eq!(got["added/a_rather_long_file_name_to_fill_the_name_table_059.txt"], b"file 59");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_resource_over_16_mb_keeps_its_size_in_its_header() {
        let d = temp("big");
        let path = d.join("big.rpf");
        fs::write(&path, build(&[("small.txt", b"s".to_vec())])).unwrap();
        let big = resource(17 << 20, 9);
        edit(&path, &[], &[Edit::Put { path: "huge.ydr".into(), data: big.clone() }], None, None).unwrap();
        assert!(Archive::open(&path, None).unwrap().read("huge.ydr").unwrap() == big);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bad_names_and_the_games_own_files_are_refused() {
        let d = temp("refuse");
        let game = d.join("GTA V");
        fs::create_dir_all(game.join("mods")).unwrap();
        fs::write(game.join("GTA5.exe"), b"x").unwrap();
        let bytes = build(&[("a.txt", b"a".to_vec())]);
        for p in [game.join("x64e.rpf"), game.join("mods").join("x64e.rpf")] {
            fs::write(&p, &bytes).unwrap();
        }
        let put = [Edit::Put { path: "b.txt".into(), data: b"b".to_vec() }];
        let mods = crate::mods::default_dir(&game);
        assert!(edit(&game.join("x64e.rpf"), &[], &put, None, Some((&game, &mods))).is_err(), "the game's own archive");
        assert!(edit(&game.join("x64e.rpf"), &[], &put, None, None).is_err(), "inside a game folder, even without being told");
        assert_eq!(fs::read(game.join("x64e.rpf")).unwrap(), bytes, "not touched");
        edit(&mods.join("x64e.rpf"), &[], &put, None, Some((&game, &mods))).unwrap();
        for bad in ["../up.txt", "a/../b.txt", "c:x.txt", "dir\\..\\x"] {
            assert!(edit(&mods.join("x64e.rpf"), &[], &[Edit::Put { path: bad.into(), data: b"x".to_vec() }], None, None).is_err(), "{bad}");
        }
        assert!(edit(&mods.join("x64e.rpf"), &[], &[Edit::Put { path: "empty.txt".into(), data: vec![] }], None, None).is_err());
        assert!(edit(&mods.join("x64e.rpf"), &[], &[Edit::Rename { path: "a.txt".into(), to: "b.txt".into() }], None, None).is_err(), "b.txt exists");
        fs::remove_dir_all(d).unwrap();
    }
}
