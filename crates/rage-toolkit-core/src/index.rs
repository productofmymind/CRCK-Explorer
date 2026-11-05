//! A searchable list of every file in the game's archives: name, size and which top-level archive (or DLC pack) it came from.
//! Built by walking the archives once (some seconds) and kept in the cache folder until the game changes (`Game::stamp`), so
//! later searches take milliseconds. Only names and sizes are read: no file is opened or decoded to build it.
use crate::{
    game::{cache_dir, Game},
    job::Job,
    search::{Kind, Query},
};
use anyhow::{bail, Result};
use std::{fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// full path like `update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/vehicles.rpf/volatus.yft`
    pub path: String,
    pub size: u32,
    source: u16,
}

impl Record {
    /// the file name, as stored (lowercase in the archives)
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn ext(&self) -> &str {
        self.name().rsplit_once('.').map(|(_, e)| e).unwrap_or("")
    }
}

/// What to keep from the matches of a text search.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// lowercase extensions without the dot; empty = any
    pub exts: Vec<String>,
    /// the top-level archive's path must contain this (lowercase), e.g. `mpheist` or `x64e`
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileIndex {
    pub stamp: String,
    sources: Vec<String>,
    records: Vec<Record>,
}

impl FileIndex {
    /// `files` are (full path, size, top-level archive)
    pub fn from_files(stamp: &str, files: impl IntoIterator<Item = (String, u32, String)>) -> Self {
        let mut ix = FileIndex { stamp: stamp.to_string(), ..Default::default() };
        for (path, size, top) in files {
            ix.push(path, size, &top);
        }
        ix
    }

    fn push(&mut self, path: String, size: u32, top: &str) {
        let source = match self.sources.iter().position(|s| s == top) {
            Some(i) => i,
            None => {
                self.sources.push(top.to_string());
                self.sources.len() - 1
            }
        } as u16;
        self.records.push(Record { path, size, source });
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn source(&self, r: &Record) -> &str {
        &self.sources[r.source as usize]
    }

    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Walk every top-level archive of the game. Stops with an error when `job` is cancelled; nothing partial is returned.
    pub fn build(game: &Game, job: &Job) -> Result<Self> {
        let tops = game.top_level_archives();
        job.set_total(tops.len());
        let mut ix = FileIndex { stamp: game.stamp(), ..Default::default() };
        for top in tops {
            if job.is_cancelled() {
                bail!("indexing cancelled");
            }
            for f in game.files_in(&top) {
                ix.push(f.path, f.size, &top);
            }
            job.step();
        }
        Ok(ix)
    }

    /// The saved index when it is for this game version, else a new one (saved). `rebuild` ignores the saved one.
    pub fn load_or_build(game: &Game, job: &Job, rebuild: bool) -> Result<Self> {
        let file = cache_dir().join("file_index.tsv");
        if !rebuild {
            if let Some(ix) = Self::load(&file) {
                if ix.stamp == game.stamp() {
                    return Ok(ix);
                }
            }
        }
        let ix = Self::build(game, job)?;
        ix.save(&file)?;
        Ok(ix)
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut s = format!("{}\n", self.stamp);
        for t in &self.sources {
            s.push_str(&format!("S\t{t}\n"));
        }
        for r in &self.records {
            s.push_str(&format!("F\t{}\t{}\t{}\n", r.source, r.size, r.path));
        }
        // written whole then renamed, so a crash never leaves half an index that would be read as complete
        let tmp = file.with_extension("tmp");
        fs::write(&tmp, s)?;
        fs::rename(&tmp, file)?;
        Ok(())
    }

    /// `None` for a missing or damaged file: the caller builds a new one
    pub fn load(file: &Path) -> Option<Self> {
        let text = fs::read_to_string(file).ok()?;
        let mut lines = text.lines();
        let mut ix = FileIndex { stamp: lines.next()?.to_string(), ..Default::default() };
        for l in lines {
            let mut p = l.splitn(4, '\t');
            match p.next()? {
                "S" => ix.sources.push(p.next()?.to_string()),
                "F" => {
                    let source: u16 = p.next()?.parse().ok()?;
                    let size = p.next()?.parse().ok()?;
                    if source as usize >= ix.sources.len() {
                        return None;
                    }
                    ix.records.push(Record { path: p.next()?.to_string(), size, source });
                }
                _ => return None,
            }
        }
        Some(ix)
    }

    fn keep(&self, r: &Record, f: &Filter) -> bool {
        (f.exts.is_empty() || f.exts.iter().any(|e| e == r.ext())) && f.source.as_ref().map_or(true, |s| self.source(r).to_lowercase().contains(s))
    }

    /// Files matching `text` (the same words, `*`/`?` patterns and path words as the Explorer search) and `filter`. With `fuzzy`,
    /// when nothing matches exactly, the names that are one or two typos away from a single word come back, nearest first.
    pub fn find(&self, text: &str, filter: &Filter, fuzzy: bool, limit: usize) -> Vec<&Record> {
        let Some(q) = Query::parse(text, Kind::Files) else { return vec![] };
        let hits: Vec<&Record> = self.records.iter().filter(|r| self.keep(r, filter) && q.matches(&r.path, false)).take(limit).collect();
        if !hits.is_empty() || !fuzzy {
            return hits;
        }
        let word = text.trim().to_lowercase();
        if word.contains(char::is_whitespace) || word.len() < 3 {
            return hits;
        }
        let max = if word.len() >= 5 { 2 } else { 1 };
        let mut near: Vec<(usize, &Record)> = self
            .records
            .iter()
            .filter(|r| self.keep(r, filter))
            .filter_map(|r| {
                let name = r.name();
                let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
                if stem.len().abs_diff(word.len()) > max {
                    return None;
                }
                edit_distance(&word, stem, max).map(|d| (d, r))
            })
            .collect();
        near.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
        near.into_iter().take(limit).map(|(_, r)| r).collect()
    }
}

/// Optimal-string-alignment distance (insert, delete, replace, swap two neighbours) when it is at most `max`.
fn edit_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    let dist = d[a.len()][b.len()];
    (dist <= max).then_some(dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FileIndex {
        let f = |p: &str, top: &str| (p.to_string(), 10, top.to_string());
        FileIndex::from_files(
            "s1",
            [
                f("x64e.rpf/levels/gta5/vehicles.rpf/adder.yft", "x64e.rpf"),
                f("x64e.rpf/levels/gta5/vehicles.rpf/adder.ytd", "x64e.rpf"),
                f("update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/props.rpf/prop_chair_01a.ydr", "update/x64/dlcpacks/mpheist/dlc.rpf"),
                f("update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/props.rpf/prop_chair_01a.ytd", "update/x64/dlcpacks/mpheist/dlc.rpf"),
            ],
        )
    }

    #[test]
    fn partial_name_and_filters() {
        let ix = sample();
        assert_eq!(ix.find("chair", &Filter::default(), false, 10).len(), 2);
        let ydr = Filter { exts: vec!["ydr".into()], ..Default::default() };
        assert_eq!(ix.find("chair", &ydr, false, 10).len(), 1);
        let dlc = Filter { source: Some("mpheist".into()), ..Default::default() };
        assert_eq!(ix.find("*.yft", &dlc, false, 10).len(), 0);
        assert_eq!(ix.find("*.yft", &Filter::default(), false, 10)[0].name(), "adder.yft");
        assert_eq!(ix.source(ix.find("chair", &ydr, false, 10)[0]), "update/x64/dlcpacks/mpheist/dlc.rpf");
    }

    #[test]
    fn typos_only_when_asked_and_nothing_matches() {
        let ix = sample();
        assert!(ix.find("addre", &Filter::default(), false, 10).is_empty());
        let hits = ix.find("addre", &Filter::default(), true, 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|r| r.name().starts_with("adder")));
        // an exact match is not mixed with near ones
        assert_eq!(ix.find("adder", &Filter::default(), true, 10).len(), 2);
        assert!(ix.find("zzzzz", &Filter::default(), true, 10).is_empty());
    }

    #[test]
    fn edit_distance_counts_swaps() {
        assert_eq!(edit_distance("adder", "adedr", 2), Some(1));
        assert_eq!(edit_distance("adder", "adder", 2), Some(0));
        assert_eq!(edit_distance("adder", "zebra", 2), None);
    }

    #[test]
    fn save_and_load_round_trip() {
        let ix = sample();
        let file = std::env::temp_dir().join(format!("crck_index_test_{}.tsv", std::process::id()));
        ix.save(&file).unwrap();
        let back = FileIndex::load(&file).unwrap();
        let _ = fs::remove_file(&file);
        assert_eq!((back.stamp.as_str(), back.len()), ("s1", 4));
        assert_eq!(back.records, ix.records);
        assert_eq!(back.sources(), ix.sources());
        fs::write(&file, "s\nF\t9\t1\tx\n").unwrap();
        assert!(FileIndex::load(&file).is_none(), "a record naming an unknown source is a damaged file");
        let _ = fs::remove_file(&file);
    }
}
