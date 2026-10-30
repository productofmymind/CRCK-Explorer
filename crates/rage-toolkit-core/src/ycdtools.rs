//! Clip dictionaries (`.ycd`): conversion to and from the `.ycd.xml` text form, and a summary.
//! The reading and writing is `rage_formats::ycd`; these wrap it for files.
use anyhow::{Context, Result};
use rage_formats::ycd::{from_xml, parse_ycd, serialize_ycd, to_xml, ClipKind};
use std::{fmt::Write as _, fs, path::{Path, PathBuf}};

/// `x.ycd` -> `x.ycd.xml`, and the other way round.
pub fn default_output(input: &Path) -> PathBuf {
    let name = input.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match name.strip_suffix(".xml") {
        Some(stem) => input.with_file_name(stem),
        None => input.with_file_name(format!("{name}.xml")),
    }
}

/// Reads a binary `.ycd` and writes it as `.ycd.xml`. Returns a one-line summary.
pub fn ycd_to_xml(input: &Path, out: &Path) -> Result<String> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let dict = parse_ycd(&bytes).with_context(|| format!("{} is not a clip dictionary this tool can read", input.display()))?;
    fs::write(out, to_xml(&dict)).with_context(|| format!("writing {}", out.display()))?;
    Ok(format!("{}: {} clip(s), {} animation(s)", out.display(), dict.clips.len(), dict.animations.len()))
}

/// Builds a binary `.ycd` from `.ycd.xml`.
pub fn ycd_from_xml(input: &Path, out: &Path) -> Result<String> {
    let xml = fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let dict = from_xml(&xml).with_context(|| format!("{}", input.display()))?;
    let bytes = serialize_ycd(&dict).with_context(|| format!("building {}", out.display()))?;
    fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    Ok(format!("{}: {} bytes, {} clip(s), {} animation(s)", out.display(), bytes.len(), dict.clips.len(), dict.animations.len()))
}

/// What is in a `.ycd` or `.ycd.xml`: every clip and animation on a line.
pub fn ycd_info(input: &Path) -> Result<String> {
    let dict = if input.to_string_lossy().to_lowercase().ends_with(".xml") {
        from_xml(&fs::read_to_string(input)?)?
    } else {
        parse_ycd(&fs::read(input)?)?
    };
    let mut s = String::new();
    for c in &dict.clips {
        let what = match &c.kind {
            ClipKind::Animation(a) => format!("plays hash_{:08X} {:.2}-{:.2}s", a.animation, a.start, a.end),
            ClipKind::List { duration, animations } => format!("{:.2}s, {} animation(s)", duration, animations.len()),
        };
        writeln!(s, "clip  {:<40} {what}, {} tag(s), {} propert(ies)", c.name, c.tags.len(), c.properties.len())?;
    }
    for a in &dict.animations {
        writeln!(s, "anim  hash_{:08X}  {} frames, {:.2}s, {} bone-tracks, {} sequence(s)", a.hash, a.frames, a.duration, a.bone_ids.len(), a.sequences.len())?;
    }
    Ok(s)
}
