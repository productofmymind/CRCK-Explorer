//! FiveM resource rules (validator "fivem" profile): manifest, stream sizes, folder structure.
//! Rule ideas come from public FiveM guides and a permissively licensed community validator's documented rule list;
//! this is an independent implementation. The size limits are community-documented figures, not measured by us.
use crate::validate::Issue;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MIB: u64 = 1024 * 1024;
const YTD_LIMIT: u64 = 16 * MIB;
const YTD_WARN: u64 = 12 * MIB;
const YFT_WARN: u64 = 25 * MIB;
const STREAM_WARN: u64 = 50 * MIB;
const STREAM_EXT: [&str; 7] = ["yft", "ytd", "ydd", "ydr", "ybn", "ymap", "ytyp"];
const JUNK: [&str; 3] = ["thumbs.db", ".ds_store", "desktop.ini"];
/// (meta file, data_file type)
const META_TYPES: [(&str, &str); 5] = [
    ("vehicles.meta", "VEHICLE_METADATA_FILE"),
    ("carcols.meta", "CARCOLS_FILE"),
    ("carvariations.meta", "VEHICLE_VARIATION_FILE"),
    ("handling.meta", "HANDLING_FILE"),
    ("vehiclelayouts.meta", "VEHICLE_LAYOUTS_FILE"),
];

fn rel(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn name_lc(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / MIB as f64)
}

/// text without `--` line comments (outside quotes)
pub(crate) fn strip_comments(s: &str) -> String {
    s.lines()
        .map(|l| {
            let mut q: Option<char> = None;
            let b: Vec<char> = l.chars().collect();
            let mut i = 0;
            while i < b.len() {
                match q {
                    Some(c) if b[i] == c => q = None,
                    Some(_) => {}
                    None if b[i] == '\'' || b[i] == '"' => q = Some(b[i]),
                    None if b[i] == '-' && b.get(i + 1) == Some(&'-') => return b[..i].iter().collect::<String>(),
                    None => {}
                }
                i += 1;
            }
            l.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// every quoted string in `s`, in order
pub(crate) fn quoted(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Option<(char, String)> = None;
    for c in s.chars() {
        match &mut cur {
            Some((q, buf)) if c == *q => {
                out.push(std::mem::take(buf));
                cur = None;
            }
            Some((_, buf)) => buf.push(c),
            None if c == '\'' || c == '"' => cur = Some((c, String::new())),
            None => {}
        }
    }
    out
}

/// strings inside the brace/paren block that follows `key` (e.g. `files { ... }`), or None when the key is absent
pub(crate) fn block_strings(text: &str, key: &str) -> Option<Vec<String>> {
    let mut found = None;
    let mut from = 0;
    while let Some(i) = text[from..].find(key) {
        let at = from + i;
        let before_ok = at == 0 || !text[..at].chars().next_back().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
        let rest = text[at + key.len()..].trim_start();
        if before_ok && (rest.starts_with('{') || rest.starts_with('(')) {
            let open = rest.chars().next().unwrap();
            let close = if open == '{' { '}' } else { ')' };
            let end = rest.find(close).unwrap_or(rest.len());
            found.get_or_insert_with(Vec::new).extend(quoted(&rest[..end]));
        }
        from = at + key.len();
    }
    found
}

/// `data_file 'TYPE' 'path'` declarations
pub(crate) fn data_files(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = text[from..].find("data_file") {
        let at = from + i + "data_file".len();
        let q = quoted(text[at..].lines().next().unwrap_or(""));
        if q.len() >= 2 {
            out.push((q[0].clone(), q[1].clone()));
        }
        from = at;
    }
    out
}

/// `*` = within a path segment, `**` = across folders
pub(crate) fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
            (0..=s.len()).any(|i| glob(rest, &s[i..]))
        }
        Some(b'*') => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| glob(&p[1..], &s[i..])),
        Some(c) => s.first() == Some(c) && glob(&p[1..], &s[1..]),
    }
}

fn push(issues: &mut Vec<Issue>, sev: char, code: &'static str, file: &str, msg: String, fix: &str) {
    issues.push(Issue { sev, code, file: file.into(), msg, fix: fix.into() });
}

pub fn rules(dir: &Path, all: &[PathBuf], issues: &mut Vec<Issue>) {
    let size = |p: &PathBuf| fs::metadata(p).map(|m| m.len()).unwrap_or(0);

    // ---- manifest
    let root_fx = dir.join("fxmanifest.lua");
    let root_legacy = dir.join("__resource.lua");
    let manifest_path = if root_fx.exists() { Some(root_fx) } else if root_legacy.exists() { Some(root_legacy) } else { None };
    let metas_present: Vec<&PathBuf> = all.iter().filter(|p| ext(p) == "meta").collect();
    match &manifest_path {
        None => {
            let nested: Vec<String> =
                all.iter().filter(|p| matches!(name_lc(p).as_str(), "fxmanifest.lua" | "__resource.lua")).map(|p| rel(dir, p)).collect();
            if nested.is_empty() {
                push(issues, 'E', "MANIFEST_MISSING", "fxmanifest.lua", "no fxmanifest.lua (or __resource.lua) in the pack".into(), "add an fxmanifest.lua at the resource root");
            } else {
                push(issues, 'W', "STRUCTURE_NESTED_RESOURCE", "fxmanifest.lua", format!("manifest is in a subfolder ({}), not at the root", nested[0]), "FiveM loads the folder that contains the manifest; point the validator at it or move the files up");
            }
        }
        Some(mp) => {
            let text = strip_comments(&fs::read_to_string(mp).unwrap_or_default());
            let fname = mp.file_name().unwrap().to_string_lossy().into_owned();
            if fname == "__resource.lua" {
                push(issues, 'W', "MANIFEST_LEGACY", &fname, "uses the deprecated __resource.lua".into(), "rename to fxmanifest.lua and add fx_version 'cerulean' and game 'gta5'");
            } else {
                match text.find("fx_version").map(|i| quoted(text[i..].lines().next().unwrap_or(""))) {
                    Some(q) if q.first().map(String::as_str) == Some("cerulean") => {}
                    Some(q) => push(issues, 'W', "MANIFEST_FX_VERSION", &fname, format!("fx_version is '{}', expected 'cerulean'", q.first().cloned().unwrap_or_default()), "use fx_version 'cerulean'"),
                    None => push(issues, 'W', "MANIFEST_FX_VERSION", &fname, "fx_version is not set".into(), "add fx_version 'cerulean'"),
                }
                if !text.contains("game") || !quoted(&text).iter().any(|q| q == "gta5") {
                    push(issues, 'W', "MANIFEST_GAME", &fname, "game 'gta5' is not declared".into(), "add game 'gta5'");
                }
            }
            let files = block_strings(&text, "files");
            let declared = data_files(&text);
            if files.is_none() && !metas_present.is_empty() {
                push(issues, 'E', "MANIFEST_NO_FILES", &fname, "meta files exist but the manifest has no files { } block".into(), "list them in files { 'data/*.meta' }");
            }
            for m in &metas_present {
                let r = rel(dir, m);
                if let Some(f) = &files {
                    if !f.iter().any(|g| glob(g.as_bytes(), r.as_bytes())) {
                        push(issues, 'E', "MANIFEST_META_NOT_IN_FILES", &fname, format!("{r} is not matched by any files {{ }} entry"), "add it to files { } or use a wildcard like 'data/*.meta'");
                    }
                }
            }
            for (meta, ty) in META_TYPES {
                if let Some(p) = metas_present.iter().find(|p| name_lc(p) == meta) {
                    let r = rel(dir, p);
                    if !declared.iter().any(|(t, path)| t == ty && path == &r) {
                        push(issues, 'E', "MANIFEST_NO_DATA_FILE", &fname, format!("no data_file '{ty}' '{r}'"), &format!("add: data_file '{ty}' '{r}'"));
                    }
                }
            }
            for (t, path) in &declared {
                if !dir.join(path).exists() {
                    push(issues, 'E', "MANIFEST_DATA_FILE_MISSING", &fname, format!("data_file '{t}' points at {path}, which does not exist"), "fix the path or add the file");
                }
            }
        }
    }

    // ---- stream
    let model_files: Vec<&PathBuf> = all.iter().filter(|p| STREAM_EXT.contains(&ext(p).as_str())).collect();
    let in_stream: Vec<&&PathBuf> = model_files.iter().filter(|p| rel(dir, p).starts_with("stream/")).collect();
    let outside: Vec<String> = model_files.iter().filter(|p| !rel(dir, p).starts_with("stream/")).map(|p| rel(dir, p)).collect();
    if !outside.is_empty() {
        push(issues, 'W', "STRUCTURE_STREAM_IN_ROOT", "stream", format!("{} model/texture file(s) outside stream/, e.g. {}", outside.len(), outside[0]), "FiveM only streams files under stream/; move them there");
    }
    if in_stream.is_empty() && !metas_present.is_empty() {
        push(issues, 'E', "STREAM_NO_FILES", "stream", "no files under stream/ (the vehicle would be invisible)".into(), "put the .yft/.ytd files in stream/");
    }
    let has = |e: &str| in_stream.iter().any(|p| ext(p) == e);
    if has("yft") && !has("ytd") {
        push(issues, 'E', "STREAM_NO_YTD", "stream", "models in stream/ but no .ytd texture dictionary".into(), "add the .ytd");
    }
    let names: Vec<String> = in_stream.iter().map(|p| name_lc(p)).collect();
    for n in &names {
        if let Some(base) = n.strip_suffix("_hi.yft") {
            if !names.contains(&format!("{base}.yft")) {
                push(issues, 'W', "STREAM_HI_NO_BASE", "stream", format!("{n} has no base {base}.yft"), "add the base fragment or rename");
            }
        }
        if let Some(base) = n.strip_suffix("+hi.ytd") {
            if !names.contains(&format!("{base}.ytd")) {
                push(issues, 'W', "STREAM_HI_NO_BASE", "stream", format!("{n} has no base {base}.ytd"), "add the base texture dictionary");
            }
        }
    }
    let mut total = 0u64;
    for p in &in_stream {
        let n = size(p);
        total += n;
        let r = rel(dir, p);
        match ext(p).as_str() {
            "ytd" if n > YTD_LIMIT => push(issues, 'E', "YTD_OVER_LIMIT", "stream", format!("{r} is {} (over the ~16 MB FiveM streaming limit)", mb(n)), "downscale or recompress textures; split into several .ytd"),
            "ytd" if n >= YTD_WARN => push(issues, 'W', "YTD_NEAR_LIMIT", "stream", format!("{r} is {} (close to the ~16 MB limit)", mb(n)), "consider smaller textures"),
            "yft" if n > YFT_WARN => push(issues, 'W', "YFT_LARGE", "stream", format!("{r} is {}", mb(n)), "very large fragment: check for duplicated geometry or too many LOD0 triangles"),
            _ => {}
        }
    }
    if total > STREAM_WARN {
        push(issues, 'W', "STREAM_TOTAL_LARGE", "stream", format!("stream/ totals {} across {} file(s)", mb(total), in_stream.len()), "large resources slow joining; optimise or split");
    }

    // ---- junk
    let junk: Vec<String> = all.iter().filter(|p| JUNK.contains(&name_lc(p).as_str()) || name_lc(p).starts_with("~$")).map(|p| rel(dir, p)).collect();
    if !junk.is_empty() {
        push(issues, 'W', "STRUCTURE_JUNK_FILES", "structure", format!("{} junk file(s), e.g. {}", junk.len(), junk[0]), "delete them before shipping");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_cases() {
        assert!(glob(b"data/*.meta", b"data/vehicles.meta"));
        assert!(!glob(b"data/*.meta", b"data/sub/vehicles.meta"));
        assert!(glob(b"data/**/*.meta", b"data/sub/vehicles.meta"));
        assert!(glob(b"**/*.meta", b"data/sub/x.meta"));
        assert!(!glob(b"data/*.meta", b"data/vehicles.xml"));
    }

    #[test]
    fn manifest_parsing() {
        let t = strip_comments("files { 'data/*.meta', -- 'x'\n \"b.meta\" }\ndata_file 'HANDLING_FILE' 'data/handling.meta'");
        assert_eq!(block_strings(&t, "files").unwrap(), vec!["data/*.meta", "b.meta"]);
        assert_eq!(data_files(&t), vec![("HANDLING_FILE".to_string(), "data/handling.meta".to_string())]);
        assert!(block_strings(&t, "client_scripts").is_none());
    }
}
