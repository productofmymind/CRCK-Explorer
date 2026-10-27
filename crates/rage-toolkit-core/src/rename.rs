//! Batch renaming: a list of files, a sort order and a list of steps in; a preview (every file's new name and whether it can be
//! applied) out; then a safe apply with an undo.
//!
//! The engine is built for GTA V asset names (`jbib_diff_001_a_uni.ytd`: component, kind, drawable number, texture letter, race)
//! but works on any files. Safety rules, all tested: nothing is overwritten (a new name that is already taken by a file outside the
//! batch, or given twice inside it, is a conflict and the batch is refused); names are checked for characters and names Windows
//! rejects; swaps and chains (a -> b, b -> a) work, because every file first moves to a temporary name; a failure half way rolls the
//! batch back; and the moves are logged so they can be undone (the log is a small text file, so undo survives a restart).
use anyhow::{bail, Context, Result};
use regex_lite::{Regex, RegexBuilder};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

// ---- steps ------------------------------------------------------------------------------------------------------------

/// Which `_`-separated part of a name a step works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// the part the step is about: a single letter (`Letters`), the first all-digit part (`Renumber`)
    Auto,
    /// by position: 0 is the first part, -1 the last
    Index(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Lower,
    Upper,
    /// first letter of every word (words are split at `_`, `-` and spaces)
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqMode {
    /// the name becomes prefix + number + suffix
    Replace,
    /// prefix + number + suffix is added after the old name
    Append,
}

/// One renaming step. Steps run in order, each on the result of the one before.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// numbers in sort order: `001`, `002`, ...
    Sequential { start: u64, digits: usize, prefix: String, suffix: String, mode: SeqMode, restart_per_folder: bool },
    /// GTA V texture letters: files that differ only in one single-letter part (`a`, `c`, `f`) get consecutive letters (`a`, `b`, `c`),
    /// the rest of the name untouched. Groups are files of one folder and extension with the same name apart from that letter.
    Letters { token: Token },
    /// GTA V drawable numbers: the distinct numbers used with one component prefix (`jbib`) are renumbered consecutively from
    /// `start`, in order, in every file of that prefix (drawables and textures follow the same mapping, so they stay matched).
    Renumber { token: Token, start: u64 },
    /// find and replace in the name (not the extension), plain or as a regular expression (`$1` refers to groups)
    Replace { find: String, with: String, regex: bool, case_sensitive: bool },
    AddPrefix(String),
    AddSuffix(String),
    RemovePrefix(String),
    RemoveSuffix(String),
    Case(Case),
    /// a template for the whole name: `{name}` the old name, `{ext}`, `{parent}` the folder's name, `{n}` the counter from 1
    /// (`{n:3}` padded to 3 digits), and `|upper` / `|lower` on any of them (`{name|upper}`)
    Pattern(String),
}

impl Op {
    /// a short description for lists and logs
    pub fn describe(&self) -> String {
        match self {
            Op::Sequential { start, digits, prefix, suffix, mode, .. } => format!("number from {start:0>d$} ({} digits){}{}{}", digits, if prefix.is_empty() { String::new() } else { format!(", prefix {prefix}") }, if suffix.is_empty() { String::new() } else { format!(", suffix {suffix}") }, if *mode == SeqMode::Append { ", after the old name" } else { "" }, d = *digits),
            Op::Letters { .. } => "texture letters a, b, c, ...".into(),
            Op::Renumber { start, .. } => format!("drawable numbers from {start}"),
            Op::Replace { find, with, regex, .. } => format!("replace {}{find:?} with {with:?}", if *regex { "(regex) " } else { "" }),
            Op::AddPrefix(p) => format!("add prefix {p:?}"),
            Op::AddSuffix(s) => format!("add suffix {s:?}"),
            Op::RemovePrefix(p) => format!("remove prefix {p:?}"),
            Op::RemoveSuffix(s) => format!("remove suffix {s:?}"),
            Op::Case(c) => format!("{c:?} case"),
            Op::Pattern(p) => format!("pattern {p}"),
        }
    }
}

/// Ready-made step lists for common GTA V jobs: (name, what it does, steps).
pub fn presets() -> Vec<(&'static str, &'static str, Vec<Op>)> {
    vec![
        ("Clothing textures: number 001 and letters a, b, c...", "hand_diff_000_a_uni, _c_, _f_ become hand_diff_001_a_uni, _b_, _c_ (change the start number in step 1)", vec![Op::Renumber { token: Token::Auto, start: 1 }, Op::Letters { token: Token::Auto }]),
        ("Clothing textures: letters a, b, c...", "jbib_diff_001_a_uni, _c_, _f_ become _a_, _b_, _c_; the rest of each name stays", vec![Op::Letters { token: Token::Auto }]),
        ("Drawable numbers: close the gaps", "jbib_000_u, jbib_003_u, jbib_007_u become 000, 001, 002 - textures with the same numbers follow", vec![Op::Renumber { token: Token::Auto, start: 0 }]),
        ("Numbers 001, 002, 003...", "every file gets the next number, in the chosen order", vec![Op::Sequential { start: 1, digits: 3, prefix: String::new(), suffix: String::new(), mode: SeqMode::Replace, restart_per_folder: false }]),
        ("Lowercase everything", "the game looks names up in lowercase", vec![Op::Case(Case::Lower)]),
        ("Spaces to underscores", "my texture 1.dds becomes my_texture_1.dds", vec![Op::Replace { find: " ".into(), with: "_".into(), regex: false, case_sensitive: true }]),
        ("Remove a _hi suffix", "model_hi.yft becomes model.yft", vec![Op::RemoveSuffix("_hi".into())]),
    ]
}

// ---- sorting ----------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// the order the files were given in
    Given,
    /// A-Z, ignoring case: `file10` before `file2`
    Alphabetical,
    /// like Alphabetical, but numbers inside names compare as numbers: `file2` before `file10`
    Natural,
    /// by the first number in the name, then by name
    Numeric,
    /// by extension, then naturally by name
    Extension,
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// natural order of two strings, ignoring case
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (na, nb): (String, String) = (a[si..i].iter().collect(), b[sj..j].iter().collect());
            let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
            let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            if a[i] != b[j] {
                return a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn first_number(s: &str) -> Option<u64> {
    let digits: String = s.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn sort_files(files: &mut [PathBuf], sort: Sort, reverse: bool) {
    match sort {
        Sort::Given => {}
        Sort::Alphabetical => files.sort_by_key(|p| file_name(p).to_lowercase()),
        Sort::Natural => files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b))),
        Sort::Numeric => files.sort_by(|a, b| {
            let (na, nb) = (file_name(a), file_name(b));
            first_number(&na).cmp(&first_number(&nb)).then_with(|| natural_cmp(&na, &nb))
        }),
        Sort::Extension => files.sort_by(|a, b| {
            let ext = |p: &Path| p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            ext(a).cmp(&ext(b)).then_with(|| natural_cmp(&file_name(a), &file_name(b)))
        }),
    }
    if reverse {
        files.reverse();
    }
}

// ---- the plan ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// the name does not change
    Unchanged,
    Ready,
    /// would collide with another file (the text says which)
    Conflict(String),
    /// the new name cannot be a file name on Windows (the text says why)
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub from: PathBuf,
    pub to: PathBuf,
    pub status: Status,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub rows: Vec<Row>,
    /// problems with the steps themselves (a bad regular expression, an unknown `{token}`, more than 26 letters, ...)
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn ready(&self) -> usize {
        self.rows.iter().filter(|r| r.status == Status::Ready).count()
    }

    pub fn problems(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r.status, Status::Conflict(_) | Status::Invalid(_))).count()
    }

    /// something to do, and nothing in the way
    pub fn can_apply(&self) -> bool {
        self.ready() > 0 && self.problems() == 0 && self.warnings.is_empty()
    }
}

/// A file while the steps work on it.
struct Work {
    from: PathBuf,
    dir: PathBuf,
    stem: String,
    ext: String,
}

fn split_name(p: &Path) -> (String, String) {
    let name = file_name(p);
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i + 1..].to_string()),
        _ => (name, String::new()),
    }
}

fn token_pos(tokens: &[&str], sel: Token, want: impl Fn(&str) -> bool) -> Option<usize> {
    match sel {
        // the last matching part from the second on (the first part is the component: `jbib`, `uppr`)
        Token::Auto => (1..tokens.len()).rev().find(|&i| want(tokens[i])),
        Token::Index(i) => {
            let idx = if i < 0 { tokens.len() as i64 + i as i64 } else { i as i64 };
            (idx >= 0 && (idx as usize) < tokens.len()).then_some(idx as usize).filter(|&k| want(tokens[k]))
        }
    }
}

fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = true;
    for c in s.chars() {
        if up && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.extend(c.to_lowercase());
        }
        if matches!(c, '_' | '-' | ' ') {
            up = true;
        }
    }
    out
}

fn apply_op(op: &Op, items: &mut [Work], warnings: &mut Vec<String>) {
    match op {
        Op::Sequential { start, digits, prefix, suffix, mode, restart_per_folder } => {
            let mut counters: HashMap<PathBuf, u64> = HashMap::new();
            let mut n = *start;
            for w in items.iter_mut() {
                let counter = if *restart_per_folder { counters.entry(w.dir.clone()).or_insert(*start) } else { &mut n };
                let num = format!("{:0>d$}", *counter, d = *digits);
                *counter += 1;
                w.stem = match mode {
                    SeqMode::Replace => format!("{prefix}{num}{suffix}"),
                    SeqMode::Append => format!("{}{prefix}{num}{suffix}", w.stem),
                };
            }
        }
        Op::Letters { token } => {
            // group key: folder, extension, the name with the letter part blanked, and where it is
            let mut groups: BTreeMap<(PathBuf, String, String, usize), Vec<(usize, char)>> = BTreeMap::new();
            for (i, w) in items.iter().enumerate() {
                let parts: Vec<&str> = w.stem.split('_').collect();
                let single = |t: &str| t.chars().count() == 1 && t.chars().all(|c| c.is_ascii_alphabetic());
                let found = match token {
                    // a texture letter follows the drawable number and is followed by the race (jbib_diff_001_a_uni), or ends a
                    // `diff` name (p_head_diff_000_a); the `u` of a drawable (jbib_000_u) is the race, not a variation letter
                    Token::Auto => (1..parts.len()).rev().find(|&i| single(parts[i]) && parts[i - 1].chars().all(|c| c.is_ascii_digit()) && !parts[i - 1].is_empty() && (i + 1 < parts.len() || parts.contains(&"diff"))),
                    Token::Index(_) => token_pos(&parts, *token, single),
                };
                let Some(k) = found else { continue };
                let mut blank = parts.clone();
                blank[k] = "";
                groups.entry((w.dir.clone(), w.ext.to_lowercase(), blank.join("_"), k)).or_default().push((i, parts[k].chars().next().unwrap()));
            }
            for ((_, _, _, k), members) in groups {
                let mut letters: Vec<char> = members.iter().map(|m| m.1.to_ascii_lowercase()).collect();
                letters.sort();
                letters.dedup();
                if letters.len() > 26 {
                    warnings.push("more than 26 texture variations with the same name: letters run out after z".into());
                    continue;
                }
                for (i, ch) in members {
                    let rank = letters.iter().position(|l| *l == ch.to_ascii_lowercase()).unwrap_or(0) as u8;
                    let new = (b'a' + rank) as char;
                    let new = if ch.is_ascii_uppercase() { new.to_ascii_uppercase() } else { new };
                    let mut parts: Vec<String> = items[i].stem.split('_').map(str::to_string).collect();
                    parts[k] = new.to_string();
                    items[i].stem = parts.join("_");
                }
            }
        }
        Op::Renumber { token, start } => {
            // one mapping per folder and component prefix, from the distinct numbers used with it
            let mut numbers: BTreeMap<(PathBuf, String), Vec<u64>> = BTreeMap::new();
            let mut at: Vec<Option<(usize, u64, usize)>> = vec![None; items.len()];
            for (i, w) in items.iter().enumerate() {
                let parts: Vec<&str> = w.stem.split('_').collect();
                let Some(k) = token_pos(&parts, *token, |t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())) else { continue };
                let Ok(n) = parts[k].parse::<u64>() else { continue };
                at[i] = Some((k, n, parts[k].len()));
                numbers.entry((w.dir.clone(), parts[0].to_lowercase())).or_default().push(n);
            }
            for v in numbers.values_mut() {
                v.sort();
                v.dedup();
            }
            for (i, w) in items.iter_mut().enumerate() {
                let Some((k, n, width)) = at[i] else { continue };
                let parts: Vec<&str> = w.stem.split('_').collect();
                let list = &numbers[&(w.dir.clone(), parts[0].to_lowercase())];
                let new = *start + list.iter().position(|x| *x == n).unwrap_or(0) as u64;
                let mut owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
                owned[k] = format!("{:0>w$}", new, w = width);
                w.stem = owned.join("_");
            }
        }
        Op::Replace { find, with, regex, case_sensitive } => {
            if find.is_empty() {
                return;
            }
            if *regex {
                match RegexBuilder::new(find).case_insensitive(!*case_sensitive).build() {
                    Ok(re) => {
                        for w in items.iter_mut() {
                            w.stem = re.replace_all(&w.stem, with.as_str()).into_owned();
                        }
                    }
                    Err(e) => warnings.push(format!("the regular expression {find:?} is not valid: {e}")),
                }
            } else {
                match RegexBuilder::new(&regex_lite::escape(find)).case_insensitive(!*case_sensitive).build() {
                    Ok(re) => {
                        // a plain replacement: `$` in it is a dollar sign
                        let with = with.replace('$', "$$");
                        for w in items.iter_mut() {
                            w.stem = re.replace_all(&w.stem, with.as_str()).into_owned();
                        }
                    }
                    Err(e) => warnings.push(format!("cannot search for {find:?}: {e}")),
                }
            }
        }
        Op::AddPrefix(p) => items.iter_mut().for_each(|w| w.stem = format!("{p}{}", w.stem)),
        Op::AddSuffix(s) => items.iter_mut().for_each(|w| w.stem = format!("{}{s}", w.stem)),
        Op::RemovePrefix(p) => {
            if !p.is_empty() {
                items.iter_mut().for_each(|w| {
                    if let Some(r) = w.stem.strip_prefix(p.as_str()) {
                        w.stem = r.to_string();
                    }
                })
            }
        }
        Op::RemoveSuffix(s) => {
            if !s.is_empty() {
                items.iter_mut().for_each(|w| {
                    if let Some(r) = w.stem.strip_suffix(s.as_str()) {
                        w.stem = r.to_string();
                    }
                })
            }
        }
        Op::Case(c) => items.iter_mut().for_each(|w| {
            w.stem = match c {
                Case::Lower => w.stem.to_lowercase(),
                Case::Upper => w.stem.to_uppercase(),
                Case::Title => title(&w.stem),
            }
        }),
        Op::Pattern(pat) => {
            for (i, w) in items.iter_mut().enumerate() {
                match expand_pattern(pat, w, i as u64 + 1) {
                    Ok(s) => w.stem = s,
                    Err(e) => {
                        warnings.push(e);
                        return;
                    }
                }
            }
        }
    }
}

fn expand_pattern(pat: &str, w: &Work, n: u64) -> std::result::Result<String, String> {
    let mut out = String::new();
    let mut rest = pat;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else { return Err(format!("the pattern {pat:?} has a '{{' without a '}}'")) };
        let body = &rest[open + 1..open + close];
        let (name, modifier) = match body.split_once('|') {
            Some((a, b)) => (a, Some(b)),
            None => (body, None),
        };
        let value = if name == "name" {
            w.stem.clone()
        } else if name == "ext" {
            w.ext.clone()
        } else if name == "parent" {
            w.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        } else if name == "n" {
            n.to_string()
        } else if let Some(width) = name.strip_prefix("n:") {
            let width: usize = width.parse().map_err(|_| format!("{{{name}}}: the width must be a number, like {{n:3}}"))?;
            format!("{:0>w$}", n, w = width)
        } else {
            return Err(format!("{{{name}}} is not known; use {{name}}, {{ext}}, {{parent}}, {{n}} or {{n:3}}"));
        };
        out.push_str(&match modifier {
            None => value,
            Some("upper") => value.to_uppercase(),
            Some("lower") => value.to_lowercase(),
            Some(m) => return Err(format!("|{m} is not known; use |upper or |lower")),
        });
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

const RESERVED: [&str; 22] = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];

/// why `name` cannot be a Windows file name, if it cannot
pub fn invalid_reason(name: &str) -> Option<String> {
    if name.is_empty() || name.starts_with('.') && name.len() == 1 {
        return Some("the name is empty".into());
    }
    if let Some(c) = name.chars().find(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (*c as u32) < 32) {
        return Some(format!("'{}' cannot be used in a file name", if (c as u32) < 32 { '?' } else { c }));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("a file name cannot end with a dot or a space".into());
    }
    let stem = name.split('.').next().unwrap_or(name).to_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Some(format!("{stem} is a reserved name on Windows"));
    }
    if name.chars().count() > 255 {
        return Some("the name is longer than 255 characters".into());
    }
    None
}

fn key(p: &Path) -> String {
    // Windows compares names without regard to case
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Work out every new name and check the batch. Nothing on disk changes.
pub fn plan(files: &[PathBuf], sort: Sort, reverse: bool, ops: &[Op]) -> Plan {
    let mut sorted: Vec<PathBuf> = files.to_vec();
    let mut seen = HashSet::new();
    sorted.retain(|p| seen.insert(key(p)));
    sort_files(&mut sorted, sort, reverse);
    let mut items: Vec<Work> = sorted
        .iter()
        .map(|p| {
            let (stem, ext) = split_name(p);
            Work { from: p.clone(), dir: p.parent().map(Path::to_path_buf).unwrap_or_default(), stem, ext }
        })
        .collect();
    let mut warnings = vec![];
    for op in ops {
        apply_op(op, &mut items, &mut warnings);
    }
    warnings.sort();
    warnings.dedup();
    let mut rows: Vec<Row> = items
        .into_iter()
        .map(|w| {
            let name = if w.ext.is_empty() { w.stem.clone() } else { format!("{}.{}", w.stem, w.ext) };
            let to = w.dir.join(&name);
            let status = if let Some(why) = invalid_reason(&name) {
                Status::Invalid(why)
            } else if to == w.from {
                Status::Unchanged
            } else {
                Status::Ready
            };
            Row { from: w.from, to, status }
        })
        .collect();

    // conflicts: two files to one name, or a name that something outside the batch (or a file that stays) already has
    let mut owners: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.status != Status::Invalid(String::new()) && !matches!(r.status, Status::Invalid(_)) {
            owners.entry(key(&r.to)).or_default().push(i);
        }
    }
    let staying: HashSet<String> = rows.iter().filter(|r| r.status == Status::Unchanged).map(|r| key(&r.from)).collect();
    let moving: HashSet<String> = rows.iter().filter(|r| r.status == Status::Ready).map(|r| key(&r.from)).collect();
    for i in 0..rows.len() {
        if rows[i].status != Status::Ready {
            continue;
        }
        let k = key(&rows[i].to);
        let same_file = key(&rows[i].from) == k; // only the case changes
        let dup = owners.get(&k).map(|v| v.len()).unwrap_or(0) > 1;
        if dup {
            let other = owners[&k].iter().find(|&&j| j != i).map(|&j| file_name(&rows[j].from)).unwrap_or_default();
            rows[i].status = Status::Conflict(format!("{other} gets the same new name"));
        } else if !same_file && staying.contains(&k) {
            rows[i].status = Status::Conflict("a file with that name is in the batch and keeps it".into());
        } else if !same_file && !moving.contains(&k) && rows[i].to.exists() {
            rows[i].status = Status::Conflict("a file with that name already exists (it would be overwritten)".into());
        }
    }
    Plan { rows, warnings }
}

// ---- applying and undoing ---------------------------------------------------------------------------------------------

/// What an apply did: `(original path, path now)` for every file that moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoLog {
    pub moves: Vec<(PathBuf, PathBuf)>,
}

/// Move `(from, to)` pairs. Every file first goes to a temporary name in its folder, so swaps and chains work; if anything fails the
/// files already moved are put back. Refuses to overwrite.
fn move_all(pairs: &[(PathBuf, PathBuf)]) -> Result<()> {
    let pid = std::process::id();
    let temps: Vec<PathBuf> = pairs.iter().enumerate().map(|(i, (from, _))| from.with_file_name(format!(".rage-toolkit-rename-{pid}-{i}.tmp"))).collect();
    let mut stage1 = 0;
    let mut stage2 = 0;
    let result = (|| -> Result<()> {
        for (i, (from, _)) in pairs.iter().enumerate() {
            fs::rename(from, &temps[i]).with_context(|| format!("moving {}", from.display()))?;
            stage1 = i + 1;
        }
        for (i, (_, to)) in pairs.iter().enumerate() {
            if to.exists() {
                bail!("{} already exists; nothing was overwritten", to.display());
            }
            fs::rename(&temps[i], to).with_context(|| format!("renaming to {}", to.display()))?;
            stage2 = i + 1;
        }
        Ok(())
    })();
    if result.is_err() {
        // put everything back: finished files from their new name, the others from their temporary name
        for i in 0..stage2 {
            let _ = fs::rename(&pairs[i].1, &temps[i]);
        }
        for i in 0..stage1 {
            let _ = fs::rename(&temps[i], &pairs[i].0);
        }
    }
    result
}

/// Do the plan. Refuses unless it `can_apply`.
pub fn apply(plan: &Plan) -> Result<UndoLog> {
    if !plan.warnings.is_empty() {
        bail!("{}", plan.warnings[0]);
    }
    if plan.problems() > 0 {
        bail!("{} name(s) conflict or are not allowed; fix them first (nothing was renamed)", plan.problems());
    }
    let pairs: Vec<(PathBuf, PathBuf)> = plan.rows.iter().filter(|r| r.status == Status::Ready).map(|r| (r.from.clone(), r.to.clone())).collect();
    if pairs.is_empty() {
        bail!("nothing to rename");
    }
    for (from, _) in &pairs {
        if !from.is_file() {
            bail!("{} is not there any more", from.display());
        }
    }
    move_all(&pairs)?;
    Ok(UndoLog { moves: pairs })
}

/// Put the files of `log` back under their old names. Fails (and changes nothing) if an old name is taken or a file moved away.
pub fn undo(log: &UndoLog) -> Result<()> {
    let mut back: Vec<(PathBuf, PathBuf)> = vec![];
    let nows: HashSet<String> = log.moves.iter().map(|m| key(&m.1)).collect();
    for (orig, now) in &log.moves {
        if !now.is_file() {
            bail!("{} is not there any more, so the batch cannot be undone", now.display());
        }
        if orig.exists() && !nows.contains(&key(orig)) {
            bail!("{} exists again, so the batch cannot be undone without overwriting it", orig.display());
        }
        back.push((now.clone(), orig.clone()));
    }
    move_all(&back)
}

/// A log as text: one `original<TAB>now` line per file.
pub fn log_to_text(log: &UndoLog) -> String {
    log.moves.iter().map(|(a, b)| format!("{}\t{}\n", a.display(), b.display())).collect()
}

pub fn log_from_text(text: &str) -> UndoLog {
    UndoLog { moves: text.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (PathBuf::from(a), PathBuf::from(b))).collect() }
}

/// where the last batch is remembered, so Undo works after a restart
pub fn log_path() -> PathBuf {
    crate::game::cache_dir().parent().map(|p| p.join("rename_undo.tsv")).unwrap_or_else(|| PathBuf::from("rename_undo.tsv"))
}

// ---- helpers for the window -------------------------------------------------------------------------------------------

/// Check a regular expression for the window's inline error.
pub fn regex_error(pattern: &str) -> Option<String> {
    Regex::new(pattern).err().map(|e| e.to_string())
}

/// The files of a folder (and below it when `deep`), sorted naturally.
pub fn files_in(dir: &Path, deep: bool) -> Vec<PathBuf> {
    let mut out = vec![];
    fn walk(dir: &Path, deep: bool, out: &mut Vec<PathBuf>, depth: usize) {
        let mut kids: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        kids.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
        for k in kids {
            if k.is_dir() {
                if deep && depth < crate::search::MAX_FOLDER_DEPTH {
                    walk(&k, deep, out, depth + 1);
                }
            } else if k.is_file() && !file_name(&k).starts_with(".rage-toolkit-rename-") {
                out.push(k);
            }
        }
    }
    walk(dir, deep, &mut out, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(p: &Plan) -> Vec<String> {
        p.rows.iter().map(|r| file_name(&r.to)).collect()
    }

    fn files(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from("C:\\x\\dir").join(n)).collect()
    }

    fn seq(start: u64, digits: usize) -> Op {
        Op::Sequential { start, digits, prefix: String::new(), suffix: String::new(), mode: SeqMode::Replace, restart_per_folder: false }
    }

    #[test]
    fn numbers_follow_the_sort_order_with_prefix_suffix_and_width() {
        let f = files(&["b.png", "a10.png", "a2.png"]);
        let p = plan(&f, Sort::Natural, false, &[Op::Sequential { start: 5, digits: 3, prefix: "tex_".into(), suffix: "_d".into(), mode: SeqMode::Replace, restart_per_folder: false }]);
        assert_eq!(names(&p), ["tex_005_d.png", "tex_006_d.png", "tex_007_d.png"]);
        assert_eq!(p.rows[0].from, f[2], "a2 sorts first, naturally");
        let p = plan(&f, Sort::Alphabetical, true, &[seq(1, 2)]);
        assert_eq!(p.rows[0].from, f[0], "reversed alphabetical starts with b");
        let p = plan(&f, Sort::Given, false, &[Op::Sequential { start: 1, digits: 1, prefix: "_".into(), suffix: String::new(), mode: SeqMode::Append, restart_per_folder: false }]);
        assert_eq!(names(&p), ["b_1.png", "a10_2.png", "a2_3.png"]);
    }

    #[test]
    fn texture_letters_are_made_consecutive_and_the_rest_of_the_name_is_kept() {
        let f = files(&["jbib_diff_001_a_uni.ytd", "jbib_diff_001_c_uni.ytd", "jbib_diff_001_f_uni.ytd", "jbib_diff_002_b_uni.ytd", "jbib_diff_002_d_whi.ytd"]);
        let p = plan(&f, Sort::Given, false, &[Op::Letters { token: Token::Auto }]);
        assert_eq!(names(&p), ["jbib_diff_001_a_uni.ytd", "jbib_diff_001_b_uni.ytd", "jbib_diff_001_c_uni.ytd", "jbib_diff_002_a_uni.ytd", "jbib_diff_002_a_whi.ytd"].map(String::from), "{:?}", names(&p));
        // 002_b_uni and 002_d_whi differ in the last part, so they are different groups: each starts at a
        let p = plan(&files(&["x_diff_001_b_uni.ytd", "x_diff_001_b_uni.ydd"]), Sort::Given, false, &[Op::Letters { token: Token::Auto }]);
        assert_eq!(names(&p), ["x_diff_001_a_uni.ytd", "x_diff_001_a_uni.ydd"], "different extensions do not share letters");
        // the race letter of a drawable is not a variation letter; props end their texture names with the letter
        let p = plan(&files(&["jbib_000_u.ydd", "p_head_diff_000_a.ytd", "p_head_diff_000_c.ytd"]), Sort::Given, false, &[Op::Letters { token: Token::Auto }]);
        assert_eq!(names(&p), ["jbib_000_u.ydd", "p_head_diff_000_a.ytd", "p_head_diff_000_b.ytd"].map(String::from), "{:?}", names(&p));
        // a name with no single-letter part is left alone
        let p = plan(&files(&["readme.txt"]), Sort::Given, false, &[Op::Letters { token: Token::Auto }]);
        assert_eq!(p.rows[0].status, Status::Unchanged);
    }

    #[test]
    fn drawable_numbers_close_gaps_and_textures_follow_their_drawables() {
        let f = files(&["jbib_000_u.ydd", "jbib_003_u.ydd", "jbib_007_u.ydd", "jbib_diff_003_a_uni.ytd", "jbib_diff_007_a_uni.ytd", "uppr_005_u.ydd"]);
        let p = plan(&f, Sort::Given, false, &[Op::Renumber { token: Token::Auto, start: 0 }]);
        assert_eq!(names(&p), ["jbib_000_u.ydd", "jbib_001_u.ydd", "jbib_002_u.ydd", "jbib_diff_001_a_uni.ytd", "jbib_diff_002_a_uni.ytd", "uppr_000_u.ydd"].map(String::from), "{:?}", names(&p));
    }

    #[test]
    fn the_clothing_preset_sets_the_number_and_the_letters_together() {
        let f = files(&["hand_diff_000_r_uni.ytd", "hand_diff_000_a_uni.ytd", "hand_diff_000_c_uni.ytd"]);
        let (_, _, ops) = presets().into_iter().next().unwrap();
        let p = plan(&f, Sort::Natural, false, &ops);
        assert_eq!(names(&p), ["hand_diff_001_a_uni.ytd", "hand_diff_001_b_uni.ytd", "hand_diff_001_c_uni.ytd"], "{:?}", names(&p));
    }

    #[test]
    fn find_replace_case_prefixes_and_patterns() {
        let f = files(&["My Tex_HI.png", "Other.png"]);
        let r = |find: &str, with: &str, regex: bool, cs: bool| Op::Replace { find: find.into(), with: with.into(), regex, case_sensitive: cs };
        assert_eq!(names(&plan(&f, Sort::Given, false, &[r(" ", "_", false, true)])), ["My_Tex_HI.png", "Other.png"]);
        assert_eq!(names(&plan(&f, Sort::Given, false, &[r("hi", "lo", false, false)])), ["My Tex_lo.png", "Other.png"]);
        assert_eq!(names(&plan(&f, Sort::Given, false, &[r("hi", "lo", false, true)]))[0], "My Tex_HI.png", "case-sensitive does not match");
        assert_eq!(names(&plan(&f, Sort::Given, false, &[r(r"^(\w+) (\w+)_HI$", "${2}_$1", true, true)]))[0], "Tex_My.png");
        assert_eq!(names(&plan(&f, Sort::Given, false, &[r("a", "$1", false, true)]))[1], "Other.png", "a plain replacement is not a template");
        let bad = plan(&f, Sort::Given, false, &[r("(", "x", true, true)]);
        assert!(!bad.warnings.is_empty() && !bad.can_apply());
        assert_eq!(names(&plan(&f, Sort::Given, false, &[Op::Case(Case::Lower), Op::AddPrefix("p_".into()), Op::RemoveSuffix("_hi".into())])), ["p_my tex.png", "p_other.png"]);
        assert_eq!(names(&plan(&f, Sort::Given, false, &[Op::Case(Case::Title)]))[0], "My Tex_Hi.png");
        let pat = plan(&f, Sort::Given, false, &[Op::Pattern("{parent|upper}_{n:3}_{name|lower}".into())]);
        assert_eq!(names(&pat), ["DIR_001_my tex_hi.png", "DIR_002_other.png"]);
        assert!(!plan(&f, Sort::Given, false, &[Op::Pattern("{nope}".into())]).warnings.is_empty());
    }

    #[test]
    fn sorting_orders() {
        let mut f = files(&["file10.b", "File2.a", "file1.c"]);
        sort_files(&mut f, Sort::Alphabetical, false);
        assert_eq!(f.iter().map(|p| file_name(p)).collect::<Vec<_>>(), ["file1.c", "file10.b", "File2.a"]);
        sort_files(&mut f, Sort::Natural, false);
        assert_eq!(f.iter().map(|p| file_name(p)).collect::<Vec<_>>(), ["file1.c", "File2.a", "file10.b"]);
        sort_files(&mut f, Sort::Extension, false);
        assert_eq!(f.iter().map(|p| file_name(p)).collect::<Vec<_>>(), ["File2.a", "file10.b", "file1.c"]);
        sort_files(&mut f, Sort::Numeric, true);
        assert_eq!(f.iter().map(|p| file_name(p)).collect::<Vec<_>>(), ["file10.b", "File2.a", "file1.c"]);
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_rename_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(d: &Path, name: &str, body: &str) -> PathBuf {
        let p = d.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn conflicts_and_invalid_names_block_the_batch_and_nothing_is_overwritten() {
        let d = temp("conflicts");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        let c = put(&d, "c.txt", "C");
        // two files to one name
        let p = plan(&[a.clone(), b.clone()], Sort::Given, false, &[Op::Pattern("same".into())]);
        assert_eq!(p.problems(), 2);
        assert!(apply(&p).is_err());
        // a name taken by a file outside the batch
        let p = plan(&[a.clone()], Sort::Given, false, &[Op::Pattern("c".into())]);
        assert!(matches!(&p.rows[0].status, Status::Conflict(m) if m.contains("overwritten")), "{:?}", p.rows[0].status);
        // a name taken by a file in the batch that keeps its name
        let p = plan(&[a.clone(), c.clone()], Sort::Given, false, &[Op::Replace { find: "a".into(), with: "c".into(), regex: false, case_sensitive: true }]);
        assert!(p.problems() >= 1 && apply(&p).is_err());
        // not allowed on Windows
        for bad in ["a:b", "con", "x?"] {
            let p = plan(&[a.clone()], Sort::Given, false, &[Op::Pattern(bad.into())]);
            assert!(matches!(p.rows[0].status, Status::Invalid(_)), "{bad}");
        }
        assert!(invalid_reason("trail.").is_some() && invalid_reason("trail ").is_some() && invalid_reason("ok.txt").is_none());
        assert_eq!(fs::read_to_string(&a).unwrap() + &fs::read_to_string(&b).unwrap() + &fs::read_to_string(&c).unwrap(), "ABC");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn swaps_chains_and_case_only_renames_work_and_undo_restores_everything() {
        let d = temp("swap");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        // a -> b, b -> a: a swap
        let ops = [Op::Replace { find: "^a$|^b$".into(), with: "X".into(), regex: true, case_sensitive: true }];
        let _ = ops; // (a regex cannot swap; use the sequential order instead)
        let p = plan(&[a.clone(), b.clone()], Sort::Given, true, &[seq(1, 1)]);
        // reversed given order: b -> 1, a -> 2 ; no clash. Now chain: 1 -> 2, 2 -> 3 style
        let log = apply(&p).unwrap();
        assert_eq!((fs::read_to_string(d.join("1.txt")).unwrap(), fs::read_to_string(d.join("2.txt")).unwrap()), ("B".into(), "A".into()));
        undo(&log).unwrap();
        assert_eq!((fs::read_to_string(&a).unwrap(), fs::read_to_string(&b).unwrap()), ("A".into(), "B".into()));
        // a real swap through the engine: rename the files to each other's names
        let one = put(&d, "1.txt", "one");
        let two = put(&d, "2.txt", "two");
        let swap = Plan { rows: vec![Row { from: one.clone(), to: two.clone(), status: Status::Ready }, Row { from: two.clone(), to: one.clone(), status: Status::Ready }], warnings: vec![] };
        let log = apply(&swap).unwrap();
        assert_eq!((fs::read_to_string(&one).unwrap(), fs::read_to_string(&two).unwrap()), ("two".into(), "one".into()));
        undo(&log).unwrap();
        assert_eq!((fs::read_to_string(&one).unwrap(), fs::read_to_string(&two).unwrap()), ("one".into(), "two".into()));
        // only the case changes
        let p = plan(&[a.clone()], Sort::Given, false, &[Op::Case(Case::Upper)]);
        assert_eq!(p.rows[0].status, Status::Ready, "A.txt is a valid new name for a.txt");
        let log = apply(&p).unwrap();
        assert!(fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name() == "A.txt"));
        undo(&log).unwrap();
        assert!(fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name() == "a.txt"));
        // the log survives as text
        assert_eq!(log_from_text(&log_to_text(&log)), log);
        // undo refuses when an old name is taken again
        let p = plan(&[a.clone()], Sort::Given, false, &[Op::AddSuffix("_new".into())]);
        let log = apply(&p).unwrap();
        put(&d, "a.txt", "someone else");
        assert!(undo(&log).is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "someone else");
        assert!(d.join("a_new.txt").is_file());
        // no temporary files are left behind
        assert!(!fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".rage-toolkit-rename-")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_half_way_puts_everything_back() {
        let d = temp("rollback");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        // b's target appears after the plan was made: the second stage refuses and the first file is restored
        let p = plan(&[a.clone(), b.clone()], Sort::Given, false, &[Op::AddPrefix("n_".into())]);
        assert!(p.can_apply());
        put(&d, "n_b.txt", "intruder");
        assert!(apply(&p).is_err());
        assert_eq!((fs::read_to_string(&a).unwrap(), fs::read_to_string(&b).unwrap(), fs::read_to_string(d.join("n_b.txt")).unwrap()), ("A".into(), "B".into(), "intruder".into()));
        assert!(!d.join("n_a.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
