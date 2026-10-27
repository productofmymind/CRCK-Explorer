//! Add-on vehicle pack layouts as loose files: FiveM resource and single-player `dlc.rpf` tree.
//! Layouts follow real working packs (SP: content.xml / setup2.xml shape of a community template; FiveM: fxmanifest + data_file).
use anyhow::{bail, Result};
use roxmltree::Document;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// (file name, content.xml / data_file type)
pub const METAS: [(&str, &str); 5] = [
    ("vehicles.meta", "VEHICLE_METADATA_FILE"),
    ("carcols.meta", "CARCOLS_FILE"),
    ("carvariations.meta", "VEHICLE_VARIATION_FILE"),
    ("handling.meta", "HANDLING_FILE"),
    ("vehiclelayouts.meta", "VEHICLE_LAYOUTS_FILE"),
];
const MODEL_EXT: [&str; 5] = ["yft", "ytd", "ydd", "ydr", "ybn"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Sp,
    Fivem,
}

/// lowercase, `[a-z0-9_]` only (resource / device names)
pub fn sanitize(n: &str) -> String {
    n.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

fn files_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
    out.sort();
    Ok(out)
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn copy_into(from: &Path, to_dir: &Path) -> Result<()> {
    fs::create_dir_all(to_dir)?;
    fs::copy(from, to_dir.join(from.file_name().unwrap()))?;
    Ok(())
}

/// Build the pack for `mode` from `src` (model files + metas, flat) into `out`. Returns the main output folder.
pub fn build(mode: Mode, raw_name: &str, src: &Path, out: &Path) -> Result<PathBuf> {
    let name = sanitize(raw_name);
    let all = files_in(src)?;
    let models: Vec<&PathBuf> = all.iter().filter(|p| MODEL_EXT.contains(&ext_of(p).as_str())).collect();
    let metas: Vec<(&str, &str)> =
        METAS.iter().copied().filter(|(f, _)| all.iter().any(|p| p.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case(f))).collect();
    if models.is_empty() {
        bail!("no model files (.yft/.ytd/...) in source folder");
    }
    if metas.is_empty() {
        eprintln!("warning: no .meta files found; vehicle will not register");
    }
    let meta_path = |f: &str| src.join(f);
    match mode {
        Mode::Fivem => {
            let root = out.join(&name);
            for m in &models {
                copy_into(m, &root.join("stream"))?;
            }
            for (f, _) in &metas {
                copy_into(&meta_path(f), &root.join("data"))?;
            }
            let mut s = String::from("fx_version 'cerulean'\ngame 'gta5'\n\nfiles {\n");
            s.push_str(&metas.iter().map(|(f, _)| format!("    'data/{f}'")).collect::<Vec<_>>().join(",\n"));
            s.push_str("\n}\n\n");
            for (f, t) in &metas {
                s.push_str(&format!("data_file '{t}' 'data/{f}'\n"));
            }
            fs::write(root.join("fxmanifest.lua"), s)?;
            println!("FiveM resource: {}  ({} stream files, {} metas)", root.display(), models.len(), metas.len());
            Ok(root)
        }
        Mode::Sp => {
            let dev = format!("dlc_{name}");
            let ccs = format!("{}_AUTOGEN", name.to_uppercase());
            let rpf = out.join("dlcpacks").join(&name).join("dlc.rpf");
            for (f, _) in &metas {
                copy_into(&meta_path(f), &rpf.join("data"))?;
            }
            for m in &models {
                copy_into(m, &rpf.join("x64").join("vehicles.rpf"))?;
            }
            // (path, type, persistent)
            let mut files: Vec<(String, &str, bool)> = metas.iter().map(|(f, t)| (format!("{dev}:/data/{f}"), *t, false)).collect();
            files.push((format!("{dev}:/%PLATFORM%/vehicles.rpf"), "RPF_FILE", true));
            let items: String = files
                .iter()
                .map(|(p, t, pers)| {
                    format!(
                        "    <Item>\n      <filename>{p}</filename>\n      <fileType>{t}</fileType>\n      <overlay value=\"false\" />\n      <disabled value=\"true\" />\n      <persistent value=\"{pers}\" />\n    </Item>\n"
                    )
                })
                .collect();
            let enable: String = files.iter().map(|(p, _, _)| format!("        <Item>{p}</Item>\n")).collect();
            let content = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CDataFileMgr__ContentsOfDataFileXml>\n  <disabledFiles />\n  <includedXmlFiles />\n  <includedDataFiles />\n  <dataFiles>\n{items}  </dataFiles>\n  <contentChangeSets>\n    <Item>\n      <changeSetName>{ccs}</changeSetName>\n      <filesToDisable />\n      <filesToEnable>\n{enable}      </filesToEnable>\n      <txdToLoad />\n      <txdToUnload />\n      <residentResources />\n      <unregisterResources />\n    </Item>\n  </contentChangeSets>\n  <patchFiles />\n</CDataFileMgr__ContentsOfDataFileXml>\n"
            );
            let setup = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SSetupData>\n  <deviceName>{dev}</deviceName>\n  <datFile>content.xml</datFile>\n  <timeStamp>00/00/0000 00:00:00</timeStamp>\n  <nameHash>{name}</nameHash>\n  <contentChangeSetGroups>\n    <Item>\n      <NameHash>GROUP_STARTUP</NameHash>\n      <ContentChangeSets>\n        <Item>{ccs}</Item>\n      </ContentChangeSets>\n    </Item>\n  </contentChangeSetGroups>\n  <type>EXTRACONTENT_COMPAT_PACK</type>\n  <order value=\"9\" />\n</SSetupData>\n"
            );
            fs::write(rpf.join("content.xml"), content)?;
            fs::write(rpf.join("setup2.xml"), setup)?;
            fs::write(out.join("dlclist_entry.txt"), format!("<Item>dlcpacks:/{name}/</Item>\n"))?;
            println!(
                "SP pack folder: {}\n  next: pack x64/vehicles.rpf and dlc.rpf, put under mods/update/x64/dlcpacks/{name}/, add the line in dlclist_entry.txt to dlclist.xml",
                rpf.display()
            );
            Ok(rpf)
        }
    }
}

/// Every path in content.xml must exist in the tree. Returns the number of missing entries.
pub fn verify(dlc_rpf_dir: &Path) -> Result<usize> {
    let strip = |s: String| s.trim_start_matches('\u{feff}').to_string();
    let content = strip(fs::read_to_string(dlc_rpf_dir.join("content.xml"))?);
    let setup = strip(fs::read_to_string(dlc_rpf_dir.join("setup2.xml"))?);
    let cdoc = Document::parse(&content)?;
    let sdoc = Document::parse(&setup)?;
    let dev = sdoc.descendants().find(|n| n.has_tag_name("deviceName")).and_then(|n| n.text()).unwrap_or("").to_string();
    let mut bad = 0;
    let data_files = cdoc.descendants().find(|n| n.has_tag_name("dataFiles"));
    for item in data_files.into_iter().flat_map(|d| d.children().filter(|c| c.has_tag_name("Item"))) {
        let f = item.children().find(|c| c.has_tag_name("filename")).and_then(|c| c.text()).unwrap_or("");
        let rel = f.replace(&format!("{dev}:/"), "").replace("%PLATFORM%", "x64");
        let ok = dlc_rpf_dir.join(&rel).exists();
        println!("{} {f}", if ok { "ok     " } else { "MISSING" });
        bad += usize::from(!ok);
    }
    Ok(bad)
}
