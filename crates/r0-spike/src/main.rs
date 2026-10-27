//! R0 feasibility spike: can rpf-archive + rage-formats replace the reference library for our workflows?
use anyhow::{bail, Context, Result};
use rage_formats::ytd::{parse_ytd, serialize_ytd};
use rpf_archive::{tree::{build_directory_tree, DirNode}, RpfArchive, RpfBuilder, RpfEncryption};
use std::{fs, path::Path};

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list", rpf] => walk(&fs::read(rpf)?, "root", "", &mut |p, d| { println!("{p}  {}", d.len()); Ok(()) })?,
        ["extract", rpf, out] => {
            let out = Path::new(out);
            walk(&fs::read(rpf)?, "root", "", &mut |p, d| {
                let dest = out.join(p);
                fs::create_dir_all(dest.parent().unwrap())?;
                Ok(fs::write(dest, d)?)
            })?
        }
        ["build", dir, out] => fs::write(out, build_dir(Path::new(dir))?)?,
        ["ytd", file] => {
            for t in parse_ytd(&fs::read(file)?)? {
                println!("{} {}x{} mips={} fmt={:?} pixel_bytes={}", t.name, t.width, t.height, t.levels, t.format, t.pixel_data.len());
            }
        }
        ["ytd-dds", file, outdir] => {
            fs::create_dir_all(outdir)?;
            for t in parse_ytd(&fs::read(file)?)? { fs::write(Path::new(outdir).join(format!("{}.dds", t.name)), t.to_dds())?; }
        }
        ["ytd-rebuild", file, out] => fs::write(out, serialize_ytd(&parse_ytd(&fs::read(file)?)?)?)?,
        _ => bail!("usage: r0-spike list|extract|build|ytd|ytd-dds|ytd-rebuild ..."),
    }
    Ok(())
}

/// folder -> RPF7 (OPEN); subfolders named *.rpf become nested archives
fn build_dir(dir: &Path) -> Result<Vec<u8>> {
    let mut b = RpfBuilder::new(RpfEncryption::Open);
    add_tree(&mut b, dir, "")?;
    b.build(None)
}

fn add_tree(b: &mut RpfBuilder, dir: &Path, prefix: &str) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        if e.file_type()?.is_dir() {
            if name.to_lowercase().ends_with(".rpf") { b.add_file(&path, build_dir(&e.path())?); }
            else { add_tree(b, &e.path(), &format!("{path}/"))?; }
        } else {
            b.add_file(&path, fs::read(e.path()).with_context(|| path.clone())?);
        }
    }
    Ok(())
}

type Sink<'a> = &'a mut dyn FnMut(&str, &[u8]) -> Result<()>;

/// every file in the archive with its full path (directories and nested *.rpf included)
fn walk(data: &[u8], name: &str, prefix: &str, f: Sink) -> Result<()> {
    let ar = RpfArchive::parse(data, name, None)?;
    let root = build_directory_tree(&ar.entries);
    walk_node(&ar, data, &root, prefix, f)
}

fn walk_node(ar: &RpfArchive, data: &[u8], node: &DirNode, prefix: &str, f: Sink) -> Result<()> {
    for file in &node.files {
        let path = format!("{prefix}{}", file.name);
        let bytes = ar.extract_entry(data, &ar.entries[file.entry_index], None)?;
        if file.name.to_lowercase().ends_with(".rpf") { walk(&bytes, &file.name, &format!("{path}/"), f)?; }
        else { f(&path, &bytes)?; }
    }
    for d in &node.subdirs { walk_node(ar, data, d, &format!("{prefix}{}/", d.name), f)?; }
    Ok(())
}

