//! Read-only access to the user's own installed GTA V: finds the install, derives the archive keys from the user's own
//! `GTA5.exe`, and opens retail (encrypted) RPFs, including archives nested inside archives, without loading them into
//! memory (the files are memory-mapped). Nothing is copied out except what the caller asks for, and no key is ever
//! written anywhere but the user's own local cache folder.
use crate::search::{Hit, Kind as SearchKind, Query};
use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use rpf_archive::{crypto::keys::GtaKeys, RpfArchive, RpfEntry, RpfEntryKind};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

fn local_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("CRCK Explorer");
    // the folder of the program's earlier name: whatever the new folder does not have yet moves over (settings, keys, the
    // cache, the rename undo log), once per run
    static MOVED: std::sync::Once = std::sync::Once::new();
    MOVED.call_once(|| {
        let old = base.join("Ratchet");
        if !old.is_dir() {
            return;
        }
        if !dir.exists() {
            let _ = fs::rename(&old, &dir);
            return;
        }
        for e in fs::read_dir(&old).into_iter().flatten().flatten() {
            let to = dir.join(e.file_name());
            if !to.exists() {
                let _ = fs::rename(e.path(), to);
            }
        }
    });
    dir
}

/// where extracted game files are kept between runs (the user's own local app folder)
pub fn cache_dir() -> PathBuf {
    local_dir().join("cache")
}

fn config_file() -> PathBuf {
    local_dir().join("game_dir.txt")
}

fn is_game_dir(p: &Path) -> bool {
    p.join("GTA5.exe").is_file() && p.join("x64e.rpf").is_file()
}

/// Remember the game folder for later runs.
pub fn save_game_dir(dir: &Path) -> Result<()> {
    if !is_game_dir(dir) {
        bail!("{} does not look like a GTA V folder (GTA5.exe and x64e.rpf expected)", dir.display());
    }
    fs::create_dir_all(local_dir())?;
    fs::write(config_file(), dir.to_string_lossy().as_bytes())?;
    Ok(())
}

/// `--dir` is tried first, then the GTAV_PATH variable, the folder saved earlier, then the usual Steam / Rockstar /
/// Epic locations (including every Steam library listed in libraryfolders.vdf).
pub fn find_game_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(p) = explicit {
        candidates.push(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("GTAV_PATH") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(s) = fs::read_to_string(config_file()) {
        candidates.push(PathBuf::from(s.trim()));
    }
    let mut steam_roots = vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"C:\Program Files\Steam")];
    for r in steam_roots.clone() {
        if let Ok(vdf) = fs::read_to_string(r.join("steamapps/libraryfolders.vdf")) {
            for line in vdf.lines().filter(|l| l.contains("\"path\"")) {
                if let Some(v) = line.split('"').nth(3) {
                    steam_roots.push(PathBuf::from(v.replace("\\\\", "\\")));
                }
            }
        }
    }
    for r in steam_roots {
        candidates.push(r.join("steamapps/common/Grand Theft Auto V"));
    }
    // any drive: a Steam library or games folder at the root is the usual layout when C: is small
    for letter in 'C'..='Z' {
        for sub in [r"SteamLibrary\steamapps\common\Grand Theft Auto V", r"Steam\steamapps\common\Grand Theft Auto V", r"Games\Grand Theft Auto V", r"Rockstar Games\Grand Theft Auto V", r"Epic Games\GTAV", r"Games\Rockstar Games\Grand Theft Auto V"] {
            candidates.push(PathBuf::from(format!("{letter}:\\{sub}")));
        }
    }
    candidates.push(PathBuf::from(r"C:\Program Files\Rockstar Games\Grand Theft Auto V"));
    candidates.push(PathBuf::from(r"C:\Program Files\Epic Games\GTAV"));
    candidates.into_iter().find(|p| is_game_dir(p))
}

/// Keys derived from `GTA5.exe`, cached per executable (size + modified time) in the user's local app folder.
pub fn load_keys(game_dir: &Path) -> Result<Arc<GtaKeys>> {
    let exe = game_dir.join("GTA5.exe");
    let meta = fs::metadata(&exe).with_context(|| format!("{} not found", exe.display()))?;
    let stamp = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let cache = local_dir().join("keys").join(format!("{}_{stamp}", meta.len()));
    if cache.join("gtav_aes_key.dat").exists() {
        if let Ok(k) = GtaKeys::load_from_path(&cache) {
            return Ok(Arc::new(k));
        }
    }
    let keys = GtaKeys::extract_from_exe(&exe, Some(&cache)).context("deriving the archive keys from GTA5.exe failed (is this a retail PC GTA5.exe?)")?;
    Ok(Arc::new(keys))
}

/// A file inside the game: which archives lead to it and its path in the innermost one.
#[derive(Debug, Clone)]
pub struct GameFile {
    /// e.g. `x64e.rpf/levels/gta5/vehicles.rpf/volatus.ytd`
    pub path: String,
    pub size: u32,
}

pub struct Archive {
    map: Arc<Mmap>,
    /// None for archives the user opened from their own folders when no game is installed (unencrypted ones still work)
    keys: Option<Arc<GtaKeys>>,
    ar: RpfArchive,
    /// lowercase relative path -> entry index (files only); made the first time a file is looked up by path, since walks
    /// over the whole game (find, search) only need `names`
    files: OnceLock<HashMap<String, usize>>,
    /// original-case relative paths
    names: Vec<(String, usize)>,
}

/// Every file of an archive as (relative path, entry index): a folder's files first, then its subfolders, in the archive's
/// order (as `rpf_archive::tree` lists them). Folder ranges come from the file, so a damaged archive whose folders list
/// themselves stops after one visit per entry instead of looping.
pub(crate) fn list_files(entries: &[RpfEntry]) -> Vec<(String, usize)> {
    fn dir(entries: &[RpfEntry], start: usize, count: usize, prefix: &str, out: &mut Vec<(String, usize)>, budget: &mut usize, depth: usize) {
        let range = start.min(entries.len())..start.saturating_add(count).min(entries.len());
        let mut subdirs = vec![];
        for i in range {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            match &entries[i].kind {
                RpfEntryKind::Directory { entries_index, entries_count } => subdirs.push((i, *entries_index as usize, *entries_count as usize)),
                _ => out.push((format!("{prefix}{}", entries[i].name), i)),
            }
        }
        if depth < 128 {
            for (i, s, c) in subdirs {
                dir(entries, s, c, &format!("{prefix}{}/", entries[i].name), out, budget, depth + 1);
            }
        }
    }
    let mut out = Vec::with_capacity(entries.len());
    match entries.first().map(|e| &e.kind) {
        Some(RpfEntryKind::Directory { entries_index, entries_count }) => dir(entries, *entries_index as usize, *entries_count as usize, "", &mut out, &mut entries.len(), 0),
        // no root folder: every file at the top
        _ => out.extend(entries.iter().enumerate().filter(|(_, e)| !e.is_directory()).map(|(i, e)| (e.name.clone(), i))),
    }
    out
}

impl Archive {
    /// `size` = the archive's own length. NG decryption of the table of contents is keyed by the archive's name AND size, and
    /// the parser measures size as "everything after the start offset", so a nested archive must be parsed from a slice that
    /// ends where the archive ends.
    fn build(map: Arc<Mmap>, keys: Option<Arc<GtaKeys>>, offset: usize, size: usize, name: &str) -> Result<Self> {
        let bounded = map.get(..offset.saturating_add(size)).with_context(|| format!("{name}: archive extends past the end of its parent"))?;
        let ar = RpfArchive::parse_at(bounded, offset, name, keys.as_deref()).with_context(|| format!("opening {name}"))?;
        let names = list_files(&ar.entries);
        Ok(Self { map, keys, ar, files: OnceLock::new(), names })
    }

    /// lowercase path -> entry index
    fn index(&self) -> &HashMap<String, usize> {
        self.files.get_or_init(|| self.names.iter().map(|(p, i)| (p.to_lowercase(), *i)).collect())
    }

    pub fn open(path: &Path, keys: Option<Arc<GtaKeys>>) -> Result<Self> {
        let f = fs::File::open(path).with_context(|| path.display().to_string())?;
        // SAFETY: read-only mapping of a game file; if another process truncates it while we read, we may fault, which is the same
        // risk any reader of a file under modification has.
        let map = Arc::new(unsafe { Mmap::map(&f)? });
        let len = map.len();
        Self::build(map, keys, 0, len, &path.file_name().unwrap().to_string_lossy())
    }

    /// every file in this archive (nested archives appear as files too)
    pub fn files(&self) -> Vec<GameFile> {
        self.names.iter().map(|(p, i)| GameFile { path: p.clone(), size: entry_size(&self.ar.entries[*i].kind) }).collect()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.index().contains_key(&path.to_lowercase())
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let i = *self.index().get(&path.to_lowercase()).with_context(|| format!("{path} not found in {}", self.ar.name))?;
        self.ar.extract_entry(&self.map, &self.ar.entries[i], self.keys.as_deref())
    }

    /// a nested archive, parsed in place inside the same mapping
    pub fn child(&self, path: &str) -> Result<Archive> {
        let i = *self.index().get(&path.to_lowercase()).with_context(|| format!("{path} not found in {}", self.ar.name))?;
        self.child_at(i)
    }

    /// the nested archive at entry `i` (walks know the index: no lookup by path)
    fn child_at(&self, i: usize) -> Result<Archive> {
        let e = &self.ar.entries[i];
        let RpfEntryKind::BinaryFile { file_offset, file_size, uncompressed_size, .. } = e.kind else { bail!("{} is not an archive", e.name) };
        let size = if file_size > 0 { file_size } else { uncompressed_size } as usize;
        Self::build(self.map.clone(), self.keys.clone(), self.ar.start_offset + file_offset as usize * 512, size, &e.name)
    }
}

fn entry_size(k: &RpfEntryKind) -> u32 {
    match k {
        RpfEntryKind::BinaryFile { file_size, uncompressed_size, .. } => {
            if *file_size > 0 { *uncompressed_size.max(file_size) } else { *uncompressed_size }
        }
        RpfEntryKind::ResourceFile { file_size, .. } => *file_size,
        RpfEntryKind::Directory { .. } => 0,
    }
}

#[derive(Clone)]
pub struct Game {
    pub dir: PathBuf,
    keys: Arc<GtaKeys>,
}

/// A texture dictionary and its parents, nearest first, following `child -> parent` relationships (lowercase names, no `.ytd`).
/// The game looks a texture up in the model's own dictionary, then in the parent, then in the parent's parent, and so on.
pub fn txd_chain(parents: &HashMap<String, String>, start: &str) -> Vec<String> {
    let mut out = vec![start.to_lowercase()];
    while let Some(p) = parents.get(out.last().unwrap()) {
        if out.contains(p) {
            break;
        }
        out.push(p.clone());
    }
    out
}

impl Game {
    pub fn keys(&self) -> Arc<GtaKeys> {
        self.keys.clone()
    }

    /// The shared vehicle texture dictionaries (`vehshare*.ytd`) the vehicle shaders name, extracted once into the cache.
    pub fn shared_vehicle_textures(&self) -> Result<Vec<PathBuf>> {
        let dir = cache_dir().join("vehshare");
        let mut out: Vec<PathBuf> = vec![];
        let have: Vec<PathBuf> = fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "ytd").unwrap_or(false)).collect();
        if have.len() >= 4 {
            return Ok(have);
        }
        fs::create_dir_all(&dir)?;
        let mut seen = std::collections::HashSet::new();
        for f in self.find_where(&|leaf| leaf.starts_with("vehshare") && leaf.ends_with(".ytd")).into_iter().rev() {
            let leaf = f.path.rsplit('/').next().unwrap().to_lowercase();
            if seen.insert(leaf.clone()) {
                let p = dir.join(&leaf);
                fs::write(&p, self.read(&f.path)?)?;
                out.push(p);
            }
        }
        Ok(out)
    }

    pub fn open(explicit: Option<&Path>) -> Result<Self> {
        let dir = find_game_dir(explicit).context("GTA V folder not found: pass --dir <folder>, set GTAV_PATH, or run `crck-explorer game --dir <folder>` once")?;
        let keys = load_keys(&dir)?;
        Ok(Self { dir, keys })
    }

    /// Open `path` inside the game: a file system path relative to the game folder whose `.rpf` parts are descended into,
    /// e.g. `x64e.rpf/levels/gta5/vehicles.rpf`. Returns the innermost archive.
    pub fn archive(&self, path: &str) -> Result<Archive> {
        let parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        // longest prefix that exists on disk is the outer archive file
        let mut disk = self.dir.clone();
        let mut i = 0;
        while i < parts.len() {
            let next = disk.join(parts[i]);
            if next.exists() {
                disk = next;
                i += 1;
                if disk.is_file() {
                    break;
                }
            } else {
                break;
            }
        }
        if !disk.is_file() {
            bail!("{} is not an archive file in {}", path, self.dir.display());
        }
        let mut ar = Archive::open(&disk, Some(self.keys.clone()))?;
        let mut rel: Vec<&str> = vec![];
        for p in &parts[i..] {
            rel.push(p);
            if p.to_lowercase().ends_with(".rpf") {
                ar = ar.child(&rel.join("/"))?;
                rel.clear();
            }
        }
        if !rel.is_empty() {
            bail!("{} is a file, not an archive", rel.join("/"));
        }
        Ok(ar)
    }

    /// The archives worth searching for assets: common.rpf, x64*.rpf, update.rpf and every DLC pack.
    pub fn top_level_archives(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![];
        let mut add_dir = |d: &Path, prefix: &str| {
            let mut names: Vec<String> = fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.to_lowercase().ends_with(".rpf")).collect();
            names.sort();
            v.extend(names.into_iter().map(|n| format!("{prefix}{n}")));
        };
        add_dir(&self.dir, "");
        add_dir(&self.dir.join("update"), "update/");
        let mut packs: Vec<String> = fs::read_dir(self.dir.join("update/x64/dlcpacks")).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        packs.sort();
        for p in packs {
            if self.dir.join(format!("update/x64/dlcpacks/{p}/dlc.rpf")).is_file() {
                v.push(format!("update/x64/dlcpacks/{p}/dlc.rpf"));
            }
        }
        v
    }

    /// Every file whose leaf name satisfies `pred`, in all game archives, as full paths like
    /// `update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/vehicles.rpf/volatus.yft`. Walks nested archives; takes a few seconds.
    pub fn find_where(&self, pred: &dyn Fn(&str) -> bool) -> Vec<GameFile> {
        let mut out = vec![];
        for top in self.top_level_archives() {
            let Ok(ar) = self.archive(&top) else { continue };
            search(&ar, &top, pred, &mut out, 0);
        }
        out
    }

    pub fn find(&self, name: &str) -> Vec<GameFile> {
        let want = name.to_lowercase();
        self.find_where(&|leaf| leaf == want)
    }

    /// Search file and/or folder names in every game archive (see [`crate::search::Query`] for the syntax), stopping at `limit`
    /// hits. Archives count as folders. Paths look like `update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/vehicles.rpf`.
    pub fn search(&self, q: &Query, limit: usize) -> Vec<Hit> {
        let mut out = vec![];
        for top in self.top_level_archives() {
            if out.len() >= limit {
                break;
            }
            let Ok(ar) = self.archive(&top) else { continue };
            // the archive itself is a folder (and a file) too
            let size = fs::metadata(self.dir.join(&top)).map(|m| m.len()).unwrap_or(0);
            let as_dir = q.kind != SearchKind::Files;
            if q.matches(&top, as_dir) {
                out.push(Hit { path: top.clone(), size, is_dir: as_dir });
            }
            search_archive(&ar, &top, q, limit, &mut out, 0);
        }
        out
    }

    /// Every `vehicles.meta` in the game, merged the way the game does (later ones, DLC and updates, win):
    /// texture dictionary `child -> parent` relationships, and model name -> texture dictionary name.
    fn vehicle_meta_tables(&self, hits: &[GameFile]) -> (HashMap<String, String>, HashMap<String, String>) {
        let (mut parents, mut txd_of) = (HashMap::new(), HashMap::new());
        for f in hits.iter().filter(|f| f.path.to_lowercase().ends_with("/vehicles.meta")) {
            let Ok(bytes) = self.read(&f.path) else { continue };
            let Ok(meta) = rage_formats::vehicles::parse_vehicles_meta(&bytes) else { continue };
            let mut file_parents: HashMap<String, String> = HashMap::new();
            for r in &meta.txd_relationships {
                file_parents.entry(r.child.to_lowercase()).or_insert_with(|| r.parent.to_lowercase());
            }
            parents.extend(file_parents);
            for v in &meta.init_datas {
                txd_of.insert(v.model_name.to_lowercase(), v.txd_name.to_lowercase());
            }
        }
        (parents, txd_of)
    }

    /// Changes whenever the game is updated (size and date of GTA5.exe and update.rpf): cached results are only good for the same stamp.
    fn game_stamp(&self) -> String {
        let one = |p: PathBuf| fs::metadata(p).map(|m| format!("{}:{}", m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))).unwrap_or_default();
        format!("v1 {} {}", one(self.dir.join("GTA5.exe")), one(self.dir.join("update/update.rpf")))
    }

    /// Which texture names live in which vehicle dictionary (`vehshare*.ytd`, `vehicles_*.ytd`, every pack), as (full path,
    /// lowercase names). Built by reading them all once (some seconds), then kept in the cache folder until the game changes.
    pub fn vehicle_texture_index(&self) -> Result<Vec<(String, Vec<String>)>> {
        let stamp = self.game_stamp();
        let cache = cache_dir().join("vehicle_texture_index.txt");
        if let Ok(text) = fs::read_to_string(&cache) {
            let mut lines = text.lines();
            if lines.next() == Some(stamp.as_str()) {
                return Ok(lines.filter_map(|l| l.split_once('\t').map(|(p, n)| (p.to_string(), n.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect()))).collect());
            }
        }
        let mut out = vec![];
        for f in self.find_where(&|leaf| leaf.ends_with(".ytd") && (leaf.starts_with("vehshare") || leaf.starts_with("vehicles_"))) {
            // a dictionary of the newer game edition that does not parse is skipped: the classic one has the same names
            let Ok(bytes) = self.read(&f.path) else { continue };
            let Ok(textures) = rage_formats::parse_ytd(&bytes) else { continue };
            out.push((f.path, textures.into_iter().map(|t| t.name.to_lowercase()).collect::<Vec<_>>()));
        }
        fs::create_dir_all(cache_dir())?;
        let mut text = format!("{stamp}\n");
        for (p, n) in &out {
            text += &format!("{p}\t{}\n", n.join(","));
        }
        fs::write(&cache, text)?;
        Ok(out)
    }

    /// For texture names a model asks for but nothing loaded has: copy the vehicle dictionaries that hold them into `out_dir`
    /// (the last pack wins, a dictionary covering several names is taken once). Returns the files and the names nobody has.
    pub fn find_vehicle_textures(&self, names: &[String], out_dir: &Path) -> Result<(Vec<PathBuf>, Vec<String>)> {
        let index = self.vehicle_texture_index()?;
        let mut missing: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let (mut files, mut nobody) = (vec![], vec![]);
        while !missing.is_empty() {
            let want = missing.remove(0);
            let Some((path, have)) = index.iter().rev().find(|(_, have)| have.contains(&want)) else {
                nobody.push(want);
                continue;
            };
            missing.retain(|m| !have.contains(m));
            let leaf = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            fs::create_dir_all(out_dir)?;
            let p = out_dir.join(&leaf);
            fs::write(&p, self.read(path)?)?;
            if !files.contains(&p) {
                files.push(p);
            }
        }
        Ok((files, nobody))
    }

    /// The texture dictionaries a vehicle reads its textures from, by the game's own rule (the `txdRelationships` in
    /// `vehicles.meta`): its own dictionary, its parent, that one's parent... For the Adder that is
    /// `adder -> vehicles_supergt_interior -> vehshare`. Written into `out_dir`, nearest first, followed by the other
    /// `vehshare*.ytd` variants (army, truck, worn). The model's own `<name>.ytd` is left to the caller when the dictionary
    /// is called like the model.
    pub fn vehicle_texture_files(&self, name: &str, out_dir: &Path) -> Result<Vec<PathBuf>> {
        let n = name.to_lowercase();
        // the walk takes a second or more: a vehicle done before with the same game files is answered from its folder
        let (marker, stamp) = (out_dir.join("chain.txt"), self.game_stamp());
        if let Ok(text) = fs::read_to_string(&marker) {
            let mut lines = text.lines();
            if lines.next() == Some(stamp.as_str()) {
                let files: Vec<PathBuf> = lines.map(|l| out_dir.join(l)).collect();
                if files.iter().all(|p| p.is_file()) {
                    return Ok(files);
                }
            }
        }
        let hits = self.find_where(&|leaf| leaf == "vehicles.meta" || (leaf.ends_with(".ytd") && (leaf.starts_with("vehshare") || leaf.starts_with("vehicles_"))));
        let (parents, txd_of) = self.vehicle_meta_tables(&hits);
        let txd = txd_of.get(&n).cloned().unwrap_or_else(|| n.clone());
        let chain = txd_chain(&parents, &txd);
        fs::create_dir_all(out_dir)?;
        let mut written: Vec<PathBuf> = vec![];
        let mut save = |leaf: &str, from: &GameFile| -> Result<()> {
            let p = out_dir.join(leaf);
            if !written.contains(&p) {
                fs::write(&p, self.read(&from.path)?)?;
                written.push(p);
            }
            Ok(())
        };
        for t in chain.iter().skip(usize::from(txd == n)) {
            let leaf = format!("{t}.ytd");
            // the last match wins, like everywhere: DLC and updates override the base game
            let found = hits.iter().rev().find(|f| f.path.to_lowercase().ends_with(&format!("/{leaf}"))).cloned().or_else(|| self.find(&leaf).pop());
            if let Some(f) = found {
                save(&leaf, &f)?;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for f in hits.iter().rev().filter(|f| f.path.rsplit('/').next().is_some_and(|l| l.to_lowercase().starts_with("vehshare"))) {
            let leaf = f.path.rsplit('/').next().unwrap().to_lowercase();
            if seen.insert(leaf.clone()) {
                save(&leaf, f)?;
            }
        }
        let names: Vec<String> = written.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        fs::write(&marker, format!("{stamp}\n{}\n", names.join("\n")))?;
        Ok(written)
    }

    /// Read one file by its full path (as printed by `find`).
    pub fn read(&self, full_path: &str) -> Result<Vec<u8>> {
        let norm = full_path.replace(char::from(92u8), "/");
        let (ar_path, inner) = match norm.to_lowercase().rfind(".rpf/") {
            Some(i) => (&norm[..i + 4], &norm[i + 5..]),
            None => bail!("{full_path}: expected a path through an archive, e.g. x64e.rpf/levels/gta5/vehicles.rpf/volatus.ytd"),
        };
        self.archive(ar_path)?.read(inner)
    }

    /// Pull a vehicle's models and textures out of the game (the last match wins, so DLC / updates override the base game),
    /// plus the shared vehicle texture dictionaries (`vehshare*.ytd`) its shaders usually name. Returns the files written.
    pub fn extract_vehicle(&self, name: &str, out_dir: &Path) -> Result<Vec<PathBuf>> {
        let n = name.to_lowercase();
        let wanted = [format!("{n}.yft"), format!("{n}_hi.yft"), format!("{n}.ytd"), format!("{n}+hi.ytd")];
        let yft = self.find(&format!("{n}.yft")).pop().with_context(|| format!("{n}.yft not found in the game archives"))?;
        let dir = yft.path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        fs::create_dir_all(out_dir)?;
        let mut written = vec![];
        let ar = self.archive(&dir)?;
        for w in &wanted {
            if ar.contains(w) {
                let p = out_dir.join(w);
                fs::write(&p, ar.read(w)?)?;
                written.push(p);
            }
        }
        // the parent dictionaries the game chains to (vehicles.meta), then the shared vehshare variants
        written.extend(self.vehicle_texture_files(&n, out_dir)?);
        Ok(written)
    }
}

fn search_archive(ar: &Archive, prefix: &str, q: &Query, limit: usize, out: &mut Vec<Hit>, depth: usize) {
    // folders are not entries of their own in an archive: they are the parent paths of its files
    let mut dirs = std::collections::BTreeSet::new();
    if q.kind != SearchKind::Files {
        for (p, _) in &ar.names {
            let mut end = 0;
            while let Some(i) = p[end..].find('/') {
                end += i;
                dirs.insert(&p[..end]);
                end += 1;
            }
        }
    }
    for d in dirs {
        if out.len() >= limit {
            return;
        }
        let full = format!("{prefix}/{d}");
        if q.matches(&full, true) {
            out.push(Hit { path: full, size: 0, is_dir: true });
        }
    }
    for (p, i) in &ar.names {
        if out.len() >= limit {
            return;
        }
        let full = format!("{prefix}/{p}");
        let is_rpf = ar.ar.entries[*i].name_lower.ends_with(".rpf");
        let as_dir = is_rpf && q.kind != SearchKind::Files;
        if q.matches(&full, as_dir) {
            out.push(Hit { path: full.clone(), size: entry_size(&ar.ar.entries[*i].kind) as u64, is_dir: as_dir });
        }
        if is_rpf && depth < 4 {
            if let Ok(c) = ar.child_at(*i) {
                search_archive(&c, &full, q, limit, out, depth + 1);
            }
        }
    }
}

fn search(ar: &Archive, prefix: &str, pred: &dyn Fn(&str) -> bool, out: &mut Vec<GameFile>, depth: usize) {
    for (p, i) in &ar.names {
        // the entry's own name is the leaf, already lowercase
        let leaf = ar.ar.entries[*i].name_lower.as_str();
        if pred(leaf) {
            out.push(GameFile { path: format!("{prefix}/{p}"), size: entry_size(&ar.ar.entries[*i].kind) });
        }
        if leaf.ends_with(".rpf") && depth < 4 {
            if let Ok(c) = ar.child_at(*i) {
                search(&c, &format!("{prefix}/{p}"), pred, out, depth + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpf_archive::{tree::build_directory_tree, RpfBuilder, RpfEncryption};

    /// the listing the archive used to be walked with: the directory tree of `rpf_archive`
    fn by_tree(entries: &[RpfEntry]) -> Vec<(String, usize)> {
        fn walk(n: &rpf_archive::tree::DirNode, prefix: &str, out: &mut Vec<(String, usize)>) {
            for f in &n.files {
                out.push((format!("{prefix}{}", f.name), f.entry_index));
            }
            for d in &n.subdirs {
                walk(d, &format!("{prefix}{}/", d.name), out);
            }
        }
        let mut out = vec![];
        walk(&build_directory_tree(entries), "", &mut out);
        out
    }

    #[test]
    fn files_are_listed_like_the_directory_tree_lists_them() {
        let mut b = RpfBuilder::new(RpfEncryption::Open);
        for p in ["b.txt", "A.ytd", "x64/levels/gta5/z.yft", "x64/levels/gta5/Adder.yft", "x64/data/a.meta", "common/data/c.xml", "x64/levels/top.txt"] {
            b.add_file(p, p.as_bytes().to_vec());
        }
        let data = b.build(None).unwrap();
        let ar = RpfArchive::parse(&data, "t.rpf", None).unwrap();
        let listed = list_files(&ar.entries);
        assert_eq!(listed, by_tree(&ar.entries));
        assert_eq!(listed.len(), 7);
        assert!(listed.iter().any(|(p, _)| p == "x64/levels/gta5/Adder.yft"), "original case kept: {listed:?}");
    }

    #[test]
    fn a_folder_that_lists_itself_is_visited_once() {
        let dir = |n: &str, s: u32, c: u32| RpfEntry { name: n.into(), name_lower: n.to_lowercase(), kind: RpfEntryKind::Directory { entries_index: s, entries_count: c } };
        let file = |n: &str| RpfEntry { name: n.into(), name_lower: n.to_lowercase(), kind: RpfEntryKind::BinaryFile { file_offset: 0, file_size: 0, uncompressed_size: 1, is_encrypted: false } };
        let entries = vec![dir("", 1, 2), dir("loop", 0, 3), file("x.txt")];
        assert!(list_files(&entries).len() <= entries.len());
    }
}
