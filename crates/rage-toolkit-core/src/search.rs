//! Name search shared by the CLI and the viewer, in the spirit of the reference tool's RPF explorer search: type part of a file
//! name or a folder name and get every match in the game archives and in folders on disk.
//!
//! Query syntax: words separated by spaces, all of which must match (case does not matter).
//! - a plain word matches anywhere in the *name* (`adder` finds `adder.yft`, `adder_hi.yft`, ...),
//! - a word with `*` or `?` is a pattern for the whole name (`*.ytd`, `vehshare*`, `adder?hi*`),
//! - a word with `/` or `\` is looked for in the *whole path* (`levels/gta5/vehicles`, `dlcpacks/mpheist`).
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// file names only
    Files,
    /// folder names only (an archive counts as a folder: you can open it like one)
    Folders,
    Both,
}

#[derive(Clone, Debug)]
pub struct Query {
    terms: Vec<Term>,
    pub kind: Kind,
}

/// One word of a query, worked out once (a search tries it on hundreds of thousands of names).
#[derive(Clone, Debug)]
struct Term {
    /// lowercase, `/` separators; for a path fragment without its outer `/`, and wrapped in `*` when it is a pattern
    text: String,
    wild: bool,
    path: bool,
}

/// One match: a full path (through archives for game files, a disk path for folders on disk).
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

impl Query {
    /// `None` when the text has no words.
    pub fn parse(text: &str, kind: Kind) -> Option<Query> {
        let terms: Vec<Term> = text
            .split_whitespace()
            .map(|t| {
                let t = t.to_lowercase().replace('\\', "/");
                let (wild, path) = (t.contains(['*', '?']), t.contains('/'));
                // a path fragment is found anywhere in the path (a pattern is allowed to float, too)
                let text = match (path, wild) {
                    (true, true) => format!("*{}*", t.trim_matches('/')),
                    (true, false) => t.trim_matches('/').to_string(),
                    _ => t,
                };
                Term { text, wild, path }
            })
            .collect();
        (!terms.is_empty()).then_some(Query { terms, kind })
    }

    /// Whether an entry called `path` (separators `/` or `\`) is a match. `is_dir` says whether it is a folder (or archive).
    pub fn matches(&self, path: &str, is_dir: bool) -> bool {
        if (is_dir && self.kind == Kind::Files) || (!is_dir && self.kind == Kind::Folders) {
            return false;
        }
        // most game paths are lowercase ASCII already: no copy then
        let lowered;
        let path = if path.bytes().any(|b| b.is_ascii_uppercase() || b == b'\\' || !b.is_ascii()) {
            lowered = path.to_lowercase().replace('\\', "/");
            lowered.as_str()
        } else {
            path
        };
        let path = path.trim_end_matches('/');
        let name = path.rsplit('/').next().unwrap_or(path);
        self.terms.iter().all(|t| match (t.path, t.wild) {
            (true, true) => glob(&t.text, path),
            (true, false) => path.contains(t.text.as_str()),
            (false, true) => glob(&t.text, name),
            (false, false) => name.contains(t.text.as_str()),
        })
    }
}

/// `*` = any run of characters, `?` = any one character; the whole text has to match.
pub fn glob(pattern: &str, text: &str) -> bool {
    if pattern.is_ascii() && text.is_ascii() {
        return glob_in(pattern.as_bytes(), text.as_bytes(), b'*', b'?');
    }
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    glob_in(&p, &t, '*', '?')
}

fn glob_in<T: PartialEq + Copy>(p: &[T], t: &[T], any_run: T, any_one: T) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == any_one || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == any_run {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == any_run)
}

/// How deep the folder walks go below the folder they start in. Deeper than any real mod folder; it stops a junction or
/// symbolic link that points back up the tree from recursing until the stack overflows.
pub const MAX_FOLDER_DEPTH: usize = 32;

/// Matches below a folder on disk (not following links), stopping at `limit` hits in `out`.
pub fn search_disk(root: &Path, q: &Query, limit: usize, out: &mut Vec<Hit>) {
    fn walk(dir: &Path, q: &Query, limit: usize, out: &mut Vec<Hit>, depth: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if out.len() >= limit {
                return;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let p = e.path();
            let full = p.display().to_string();
            if ft.is_dir() {
                if q.matches(&full, true) {
                    out.push(Hit { path: full, size: 0, is_dir: true });
                }
                if depth < 24 {
                    walk(&p, q, limit, out, depth + 1);
                }
            } else if q.matches(&full, false) {
                out.push(Hit { path: full, size: e.metadata().map(|m| m.len()).unwrap_or(0), is_dir: false });
            }
        }
    }
    walk(root, q, limit, out, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str, k: Kind) -> Query {
        Query::parse(t, k).unwrap()
    }

    #[test]
    fn plain_words_match_names_not_folders_above() {
        let files = q("adder", Kind::Files);
        assert!(files.matches("x64e.rpf/levels/gta5/vehicles.rpf/adder_hi.yft", false));
        assert!(!files.matches("x64e.rpf/levels/gta5/adder/readme.txt", false), "the folder is called adder, the file is not");
        assert!(!files.matches("x64e.rpf/levels/adder", true), "folders are not files");
        assert!(Query::parse("   ", Kind::Both).is_none());
    }

    #[test]
    fn folder_search_finds_folders_and_archives() {
        let f = q("vehicles", Kind::Folders);
        assert!(f.matches("x64e.rpf/levels/gta5/vehicles.rpf", true));
        assert!(f.matches("x64e.rpf/levels/gta5/vehicles", true));
        assert!(!f.matches("x64e.rpf/levels/gta5/vehicles.rpf/adder.yft", false));
        assert!(q("vehicles", Kind::Both).matches("x64e.rpf/levels/gta5/vehicles.rpf/vehicles.meta", false));
    }

    #[test]
    fn all_words_must_match_and_paths_are_looked_for_whole() {
        assert!(q("vehshare ytd", Kind::Files).matches("a.rpf/vehshare_army.ytd", false));
        assert!(!q("vehshare ytd", Kind::Files).matches("a.rpf/vehshare_army.yft", false));
        assert!(q("dlcpacks\\mpheist", Kind::Files).matches("update/x64/dlcpacks/mpheist/dlc.rpf/x64/a.ytd", false));
        assert!(!q("dlcpacks/mpluxe", Kind::Files).matches("update/x64/dlcpacks/mpheist/dlc.rpf/x64/a.ytd", false));
    }

    #[test]
    fn patterns_match_the_whole_name() {
        assert!(glob("*.ytd", "vehshare.ytd"));
        assert!(!glob("*.ytd", "vehshare.ytd.bak"));
        assert!(glob("adder?hi*", "adder_hi.yft"));
        assert!(glob("a*b*c", "axxbyyc"));
        assert!(!glob("a*b*c", "axxbyy"));
        assert!(q("*.ytd", Kind::Files).matches("a/b/vehshare.ytd", false));
        assert!(q("levels/*vehicles*", Kind::Both).matches("x64e.rpf/levels/gta5/vehicles.rpf", true));
    }

    #[test]
    fn disk_search_walks_folders() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_search_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("Badges/LA")).unwrap();
        fs::write(d.join("Badges/LA/hand_diff_000_a_uni.ytd"), b"x").unwrap();
        fs::write(d.join("notes.txt"), b"x").unwrap();
        let (mut files, mut dirs) = (vec![], vec![]);
        search_disk(&d, &q("hand_diff", Kind::Files), 100, &mut files);
        search_disk(&d, &q("la", Kind::Folders), 100, &mut dirs);
        assert_eq!(files.len(), 1);
        assert!(!files[0].is_dir && files[0].size == 1);
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].is_dir && dirs[0].path.ends_with("LA"));
        let mut capped = vec![];
        search_disk(&d, &q("a", Kind::Both), 1, &mut capped);
        assert_eq!(capped.len(), 1);
        fs::remove_dir_all(d).unwrap();
    }

    /// A junction that points back up the tree (`mklink /J`, no administrator needed) must not make the folder walks recurse
    /// until the stack overflows.
    #[cfg(windows)]
    #[test]
    fn folder_walks_stop_at_a_junction_loop() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_loop_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a")).unwrap();
        fs::write(d.join("a").join("x.png"), b"x").unwrap();
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(d.join("a").join("loop")).arg(&d).output().map(|o| o.status.success()).unwrap_or(false);
        if !made {
            eprintln!("skipped: could not make a junction here");
            return;
        }
        let deep = crate::rename::files_in(&d, true);
        assert!(!deep.is_empty() && deep.len() <= MAX_FOLDER_DEPTH + 2, "{} files", deep.len());
        assert!(!crate::pipeline::collect_images(std::slice::from_ref(&d)).is_empty());
        let mut hits = vec![];
        search_disk(&d, &Query::parse("x.png", Kind::Files).unwrap(), 10_000, &mut hits);
        assert_eq!(hits.len(), 1, "the disk search skips links");
        // remove the junction itself first (not what it points to)
        let _ = fs::remove_dir(d.join("a").join("loop"));
        fs::remove_dir_all(d).unwrap();
    }
}
