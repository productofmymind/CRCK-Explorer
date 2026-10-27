//! RPF7 archives on top of the (vendored, patched) `rpf-archive` crate. OPEN encryption only: needs no game keys.
use anyhow::{bail, Context, Result};
use rpf_archive::{
    tree::{build_directory_tree, DirNode},
    RpfArchive, RpfBuilder, RpfEncryption,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

type Sink<'a> = &'a mut dyn FnMut(&str, &[u8]) -> Result<()>;

/// Archives inside archives deeper than this are not opened (real ones go 2 or 3 levels down). A damaged or crafted archive
/// can contain itself, which would otherwise recurse until the stack overflows.
pub const MAX_NESTING: usize = 8;

/// `base` joined with a path read from an archive. Names inside an archive are data from the file, so a part that would
/// leave `base` (`..`, a drive like `C:`, a stream like `a:b`) is refused; empty and `.` parts are skipped. Both `/` and `\`
/// separate parts.
pub fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            bail!("refusing to write {rel:?}: that name would leave the output folder");
        }
        out.push(part);
    }
    if out == base {
        bail!("refusing to write {rel:?}: not a file name");
    }
    Ok(out)
}

/// Every file in the archive with its full path (folders kept); nested `*.rpf` are descended into as folders.
pub fn walk(data: &[u8], name: &str, prefix: &str, f: Sink) -> Result<()> {
    walk_at(data, name, prefix, f, 0)
}

fn walk_at(data: &[u8], name: &str, prefix: &str, f: Sink, depth: usize) -> Result<()> {
    if depth > MAX_NESTING {
        bail!("{prefix}: archives nested more than {MAX_NESTING} deep (damaged archive?)");
    }
    let ar = RpfArchive::parse(data, name, None)?;
    walk_node(&ar, data, &build_directory_tree(&ar.entries), prefix, f, depth)
}

fn walk_node(ar: &RpfArchive, data: &[u8], node: &DirNode, prefix: &str, f: Sink, depth: usize) -> Result<()> {
    for file in &node.files {
        let path = format!("{prefix}{}", file.name);
        let bytes = ar.extract_entry(data, &ar.entries[file.entry_index], None)?;
        if file.name.to_lowercase().ends_with(".rpf") {
            walk_at(&bytes, &file.name, &format!("{path}/"), f, depth + 1)?;
        } else {
            f(&path, &bytes)?;
        }
    }
    for d in &node.subdirs {
        walk_node(ar, data, d, &format!("{prefix}{}/", d.name), f, depth)?;
    }
    Ok(())
}

pub fn list(rpf: &Path) -> Result<Vec<(String, usize)>> {
    let mut out = vec![];
    walk(&fs::read(rpf)?, "root", "", &mut |p, d| {
        out.push((p.to_string(), d.len()));
        Ok(())
    })?;
    Ok(out)
}

/// Unpack to `out`; nested archives become `*.rpf` folders so `extract -> edit -> build` round-trips.
pub fn extract(rpf: &Path, out: &Path) -> Result<usize> {
    let mut n = 0;
    walk(&fs::read(rpf)?, "root", "", &mut |p, d| {
        let dest = safe_join(out, p)?;
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::write(&dest, d).with_context(|| dest.display().to_string())?;
        n += 1;
        Ok(())
    })?;
    Ok(n)
}

/// Folder -> RPF7 bytes. Subfolders named `*.rpf` become nested archives.
pub fn build(dir: &Path) -> Result<Vec<u8>> {
    let mut b = RpfBuilder::new(RpfEncryption::Open);
    add_tree(&mut b, dir, "")?;
    b.build(None)
}

fn add_tree(b: &mut RpfBuilder, dir: &Path, prefix: &str) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        if e.file_type()?.is_dir() {
            if name.to_lowercase().ends_with(".rpf") {
                b.add_file(&path, build(&e.path())?);
            } else {
                add_tree(b, &e.path(), &format!("{path}/"))?;
            }
        } else {
            b.add_file(&path, fs::read(e.path()).with_context(|| path.clone())?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names_cannot_leave_the_output_folder() {
        let base = Path::new("out");
        assert_eq!(safe_join(base, r"a/b\c.ytd").unwrap(), base.join("a").join("b").join("c.ytd"));
        assert_eq!(safe_join(base, "/x//./y.txt").unwrap(), base.join("x").join("y.txt"));
        for bad in ["../evil.dll", r"a\..\..\evil.dll", r"C:\Windows\evil.dll", "file.txt:stream", "", "./"] {
            assert!(safe_join(base, bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn extracting_a_crafted_archive_writes_nothing_outside() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_slip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut b = RpfBuilder::new(RpfEncryption::Open);
        b.add_file("ok.txt", b"fine".to_vec());
        b.add_file("../escaped.txt", b"bad".to_vec());
        let rpf = dir.join("crafted.rpf");
        fs::write(&rpf, b.build(None).unwrap()).unwrap();
        let out = dir.join("out");
        assert!(extract(&rpf, &out).is_err());
        assert!(!dir.join("escaped.txt").exists(), "a '..' entry must not be written next to the output folder");
        fs::remove_dir_all(&dir).unwrap();
    }
}
