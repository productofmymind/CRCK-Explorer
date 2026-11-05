//! Streams the World view's models: worker threads read a model from the game, find its textures through the dictionary chain
//! (`gtxd.meta`), and hand back ready GPU buffers. Nothing is ever unloaded yet (a few thousand models and textures fit).
use crate::world_gpu::{Factory, GpuModel};
use rage_toolkit_core::{
    game::{txd_chain, Game},
    worldextra,
    worldload::WorldData,
    worldmodel::{self, Ytd},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{mpsc, Arc, Mutex},
};

/// longest side of a texture on the GPU: the view shows whole city blocks, not close-ups
/// biggest side of a texture while the pictures are under the memory budget (LOD atlases are 1,024 to 2,048 wide; 512 made distant buildings soft)
const MAX_TEXTURE: u32 = 2048;
/// the largest side kept of a block-compressed texture (a quarter to an eighth of the memory of RGBA8, so bigger ones fit)
const MAX_TEXTURE_BC: u32 = 4096;
/// once this many bytes of pictures are on the GPU, new ones are made small
const DEFAULT_TEXTURE_BUDGET: u64 = 1_800_000_000;

/// The debugging switches of the environment, read once (the loader asks on every model and every texture, from several threads).
struct DebugEnv {
    debug: bool,
    texlog: bool,
    absent: bool,
    masks: bool,
    no_proxy_fill: bool,
    no_terrain: bool,
    tex_where: Option<String>,
}

fn dbg_env() -> &'static DebugEnv {
    static D: std::sync::OnceLock<DebugEnv> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let on = |name: &str| std::env::var_os(name).is_some();
        DebugEnv {
            debug: on("CRCK_WORLD_DEBUG"),
            texlog: on("CRCK_WORLD_TEXLOG"),
            absent: on("CRCK_WORLD_ABSENT"),
            masks: on("CRCK_WORLD_MASKS"),
            no_proxy_fill: on("CRCK_WORLD_NOPROXYFILL"),
            no_terrain: on("CRCK_WORLD_NOTERRAIN"),
            tex_where: std::env::var("CRCK_WORLD_TEXWHERE").ok(),
        }
    })
}

/// Upload texture `tex` of `ytd` under `key`. With a device that reads block-compressed pictures the stored mip levels go up as they are (the
/// biggest levels dropped when the memory budget is filling); otherwise the picture is decoded, shrunk by the budget and given CPU-made mips.
/// `full` asks for the whole size whatever the memory used (a model loaded again for the camera that came close). Gives whether the picture went up
/// smaller than it could (and is marked, so a model using it can be upgraded later).
fn upload(factory: &Factory, ytd: &Ytd, tex: &str, key: &str, full: bool) -> Option<bool> {
    let reduced = upload_sized(factory, ytd, tex, key, full)?;
    if reduced {
        factory.mark_reduced(key);
    }
    Some(reduced)
}

fn upload_sized(factory: &Factory, ytd: &Ytd, tex: &str, key: &str, full: bool) -> Option<bool> {
    if factory.bc_supported() {
        let used = factory.texture_bytes();
        let budget = texture_budget();
        let skip = if full { 0 } else if used < budget * 2 / 5 { 0 } else if used < budget * 7 / 10 { 1 } else if used < budget { 2 } else { 3 };
        if let Some(c) = ytd.compressed(tex, skip, MAX_TEXTURE_BC) {
            if dbg_env().texlog {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_tex.txt")) {
                    let _ = writeln!(f, "bc {:?} {}x{} ({} levels, skipped {skip})", c.kind, c.width, c.height, c.levels.len());
                }
            }
            if factory.add_texture_bc(key, &c) {
                return Some(c.reduced);
            }
        }
    }
    let (w, h, rgba) = ytd.decode(tex)?;
    let (w, h, rgba, reduced) = shrink_budgeted(factory, w, h, rgba, full);
    factory.add_texture(key, w, h, &rgba);
    Some(reduced)
}

/// the texture budget (CRCK_WORLD_TEXBUDGET_MB overrides it, to try the limits)
fn texture_budget() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *B.get_or_init(|| std::env::var("CRCK_WORLD_TEXBUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).map_or(DEFAULT_TEXTURE_BUDGET, |mb| mb * 1_000_000))
}

/// The side a new texture may have, by how much of the budget the pictures on the GPU have used: full size while there is room, smaller as it fills
/// (each step down drops the top mip level, a quarter of the memory).
fn shrink_budgeted(factory: &Factory, w: u32, h: u32, rgba: Vec<u8>, full: bool) -> (u32, u32, Vec<u8>, bool) {
    let (w0, h0) = (w, h);
    let used = factory.texture_bytes();
    let cap = if full || used < texture_budget() * 2 / 5 {
        MAX_TEXTURE
    } else if used < texture_budget() * 7 / 10 {
        1024
    } else if used < texture_budget() {
        512
    } else {
        256
    };
    // smaller than the full size allows only because of the budget
    let reduced = cap < MAX_TEXTURE && w.max(h) > cap;
    let out = crate::gpu::shrink_to(w, h, rgba, cap);
    if dbg_env().texlog {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_tex.txt")) {
            let _ = writeln!(f, "{w0}x{h0} -> {}x{}", out.0, out.1);
        }
    }
    (out.0, out.1, out.2, reduced)
}
/// textures taken from an HD dictionary (shown in the diagnostics)
pub static HD_TEXTURES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// stop adding textures here (about 1 GB at the size above)
const MAX_TEXTURES: usize = 30000;
/// requests in flight at once
const MAX_PENDING: usize = 64;
const WORKERS: usize = 8;
/// models kept on the GPU before the ones no picture has asked for lately are dropped
const MAX_MODELS: usize = 5000;
/// pictures (passes) without a request after which a model may be dropped
const IDLE_PASSES: u64 = 300;

/// Everything a worker reads, shared and read-only.
pub struct Shared {
    pub game: Game,
    /// lowercase file name -> full game path of the model and texture files (the last copy wins)
    pub files: HashMap<String, String>,
    pub names: HashMap<u32, String>,
    pub data: Arc<WorldData>,
    pub parents: HashMap<String, String>,
    /// lowercase texture name -> the dictionary that has it, over the whole game (the fallback when the model's own chain has none)
    pub tex_index: Arc<HashMap<String, String>>,
    /// asset or dictionary name -> its HD (`+hi`) texture dictionary, from the maps' manifests
    pub hd_bind: Arc<HashMap<String, String>>,
    /// static collision files by the hash of their name -> full game path
    pub ybn_by_hash: HashMap<u32, String>,
    /// the user's own model and texture files (lowercase file name -> path on disk): they win over the game's of the same name
    pub loose: std::sync::RwLock<HashMap<String, std::path::PathBuf>>,
    /// names of those files' models, by hash (the game's name table does not know them)
    pub loose_names: std::sync::RwLock<HashMap<u32, String>>,
}

impl Shared {
    /// the bytes of a model or texture file: the user's own when they have one of that name, else the game's
    fn read_named(&self, file: &str) -> Option<Vec<u8>> {
        if let Some(p) = self.loose.read().unwrap().get(file) {
            return std::fs::read(p).ok();
        }
        self.game.read(self.files.get(file)?).ok()
    }

    fn has_file(&self, file: &str) -> bool {
        self.loose.read().unwrap().contains_key(file) || self.files.contains_key(file)
    }
}

pub struct Loader {
    shared: Arc<Shared>,
    tx: mpsc::Sender<(u32, bool)>,
    rx: mpsc::Receiver<(u32, bool, Option<(GpuModel, bool)>, bool)>,
    /// models asked for again at full texture size (the worker reads this set), and the models whose textures were cut by the memory budget
    upgrading: Arc<Mutex<HashSet<u32>>>,
    reduced: HashSet<u32>,
    /// models loaded again at full size so far
    upgraded: usize,
    /// which picture (pass) last asked for each model: a request that no recent picture wants is dropped by the workers, so what the camera
    /// looks at now is loaded first
    pass: Arc<std::sync::atomic::AtomicU64>,
    wanted: Arc<Mutex<HashMap<(u32, bool), u64>>>,
    pending: HashSet<(u32, bool)>,
    /// (archetype, is collision) -> its model (None: it has none, or could not be read)
    models: HashMap<(u32, bool), Option<Arc<GpuModel>>>,
    factory: Factory,
    /// world overlays made of lines (kind 0 path nodes, 1 navigation meshes) by (kind, cell): None while loading or when the file has nothing
    lines: HashMap<(u8, u32), Option<Arc<GpuModel>>>,
    line_tx: mpsc::Sender<((u8, u32), Option<GpuModel>)>,
    line_rx: mpsc::Receiver<((u8, u32), Option<GpuModel>)>,
    line_asked: HashSet<(u8, u32)>,
    /// answers received so far, dropped requests included: a picture made before one arrived may need making again
    arrivals: usize,
    /// skeleton bones as segments in model space, by archetype (None while loading, empty when it has none)
    skeletons: HashMap<u32, Option<Arc<Vec<([f32; 3], [f32; 3])>>>>,
    skeleton_tx: mpsc::Sender<(u32, Vec<([f32; 3], [f32; 3])>)>,
    skeleton_rx: mpsc::Receiver<(u32, Vec<([f32; 3], [f32; 3])>)>,
    skeleton_asked: HashSet<u32>,
    /// models dropped so far to stay within `MAX_MODELS`
    evicted: usize,
    /// pictures freed because no part used them any more
    evicted_textures: usize,
}

impl Loader {
    pub fn new(shared: Arc<Shared>, factory: Factory) -> Self {
        let (tx, req_rx) = mpsc::channel::<(u32, bool)>();
        let (res_tx, rx) = mpsc::channel();
        let req_rx = Arc::new(Mutex::new(req_rx));
        let ytds: Arc<Mutex<VecDeque<(String, Arc<Ytd>)>>> = Arc::new(Mutex::new(VecDeque::new()));
        let pass = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let wanted: Arc<Mutex<HashMap<(u32, bool), u64>>> = Arc::new(Mutex::new(HashMap::new()));
        let upgrading: Arc<Mutex<HashSet<u32>>> = Default::default();
        for _ in 0..WORKERS {
            let (req_rx, res_tx, shared, factory, ytds, pass, wanted, upgrading) = (req_rx.clone(), res_tx.clone(), shared.clone(), factory.clone(), ytds.clone(), pass.clone(), wanted.clone(), upgrading.clone());
            std::thread::spawn(move || loop {
                let (hash, coll) = match req_rx.lock().unwrap().recv() {
                    Ok(h) => h,
                    Err(_) => break,
                };
                let last = wanted.lock().unwrap().get(&(hash, coll)).copied().unwrap_or(0);
                let stale = pass.load(std::sync::atomic::Ordering::Relaxed) > last + 2;
                let model = if stale { None } else if coll { load_collision(&shared, &factory, hash).map(|m| (m, false)) } else { load_model(&shared, &factory, &ytds, hash, upgrading.lock().unwrap().contains(&hash)) };
                if model.is_none() && !coll && !stale && dbg_env().absent {
                    debug_absent(&shared, hash);
                }
                if res_tx.send((hash, coll, model, stale)).is_err() {
                    break;
                }
            });
        }
        let (line_tx, line_rx) = mpsc::channel();
        let (skeleton_tx, skeleton_rx) = mpsc::channel();
        Self { shared, tx, rx, upgrading, reduced: HashSet::new(), upgraded: 0, pass, wanted, pending: HashSet::new(), models: HashMap::new(), factory, lines: HashMap::new(), line_tx, line_rx, line_asked: HashSet::new(), arrivals: 0, skeletons: HashMap::new(), skeleton_tx, skeleton_rx, skeleton_asked: HashSet::new(), evicted: 0, evicted_textures: 0 }
    }

    fn ask(&mut self, hash: u32, coll: bool) -> bool {
        let k = (hash, coll);
        if self.models.contains_key(&k) {
            return true;
        }
        // still wanted by this picture, whether or not it is already queued
        self.wanted.lock().unwrap().insert(k, self.pass.load(std::sync::atomic::Ordering::Relaxed));
        if self.pending.contains(&k) {
            return true;
        }
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.insert(k);
        let _ = self.tx.send(k);
        true
    }

    /// Ask for a model unless it is known or asked for already. False when the queue is full (ask again next frame).
    pub fn want(&mut self, hash: u32) -> bool {
        self.ask(hash, false)
    }

    /// The same for an archetype's collision.
    pub fn want_collision(&mut self, hash: u32) -> bool {
        self.ask(hash, true)
    }

    /// Take what the workers finished; true when something new arrived.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((h, c, m, stale)) = self.rx.try_recv() {
            self.pending.remove(&(h, c));
            self.arrivals += 1;
            // a dropped request is not an answer: it is asked for again when a picture wants it
            if !c {
                if self.upgrading.lock().unwrap().remove(&h) && !stale && m.is_some() {
                    self.upgraded += 1;
                }
                match &m {
                    Some((_, true)) if !stale => {
                        self.reduced.insert(h);
                    }
                    _ if !stale => {
                        self.reduced.remove(&h);
                    }
                    _ => {}
                }
            }
            if !stale {
                self.models.insert((h, c), m.map(|m| Arc::new(m.0)));
            }
            any = true;
        }
        while let Ok((k, m)) = self.line_rx.try_recv() {
            self.lines.insert(k, m.map(Arc::new));
            any = true;
        }
        while let Ok((h, segs)) = self.skeleton_rx.try_recv() {
            if dbg_env().debug && !segs.is_empty() {
                eprintln!("SKELETON {:?} {} bones", self.shared.names.get(&h), segs.len());
            }
            self.skeletons.insert(h, Some(Arc::new(segs)));
            any = true;
        }
        self.evict();
        // pictures nothing draws with any more are freed once in a while, or at once when the budget is nearly used
        if (self.arrivals % 128 == 0 && self.factory.texture_count() > 1500) || self.factory.texture_bytes() > texture_budget() * 9 / 10 {
            self.evicted_textures += self.factory.evict_unused();
        }
        any
    }

    /// The bones of an archetype's model as segments in model space, read in the background: None until it is ready; empty when the model has no
    /// skeleton (most props have none).
    pub fn skeleton(&mut self, hash: u32) -> Option<Arc<Vec<([f32; 3], [f32; 3])>>> {
        if let Some(Some(s)) = self.skeletons.get(&hash) {
            return Some(s.clone());
        }
        if self.skeleton_asked.insert(hash) {
            let (shared, tx) = (self.shared.clone(), self.skeleton_tx.clone());
            std::thread::spawn(move || {
                let segs = skeleton_of(&shared, hash).unwrap_or_default();
                let _ = tx.send((hash, segs));
            });
        }
        None
    }

    /// Over the limit, drop the models no recent picture has asked for (they are loaded again when the camera comes back). What the current view
    /// uses is asked for every picture, so it stays.
    fn evict(&mut self) {
        if self.models.len() <= MAX_MODELS {
            return;
        }
        let now = self.pass.load(std::sync::atomic::Ordering::Relaxed);
        let mut wanted = self.wanted.lock().unwrap();
        let idle: Vec<(u32, bool)> = self.models.iter().filter(|(k, m)| m.is_some() && wanted.get(k).map_or(true, |&p| p + IDLE_PASSES < now)).map(|(k, _)| *k).collect();
        for k in &idle {
            self.models.remove(k);
            wanted.remove(k);
        }
        self.evicted += idle.len();
    }

    /// how many pictures were freed, and how many bytes are on the GPU
    pub fn texture_stats(&self) -> (usize, u64, usize) {
        (self.evicted_textures, self.factory.texture_bytes(), self.factory.texture_count())
    }

    /// how many models were dropped to stay within the limit
    pub fn evicted(&self) -> usize {
        self.evicted
    }

    /// The line overlay of one grid cell (kind 0: path nodes, cell = the `nodes<cell>.ynd` number; kind 1: navigation mesh, cell = x * 100 + y of the
    /// 150 m grid), read in the background. None until it is ready, and when the cell has no file.
    pub fn lines(&mut self, kind: u8, cell: u32) -> Option<Arc<GpuModel>> {
        let k = (kind, cell);
        if let Some(m) = self.lines.get(&k) {
            return m.clone();
        }
        if self.line_asked.insert(k) {
            let (shared, factory, tx) = (self.shared.clone(), self.factory.clone(), self.line_tx.clone());
            std::thread::spawn(move || {
                let (name, colour) = if kind == 0 {
                    (format!("nodes{cell}.ynd"), [255, 200, 40, 255])
                } else {
                    (format!("navmesh[{}][{}].ynv", cell / 100 * 3, cell % 100 * 3), [60, 200, 255, 255])
                };
                let model = shared.files.get(&name).and_then(|p| shared.game.read(p).ok()).and_then(|b| if kind == 0 { worldextra::path_segments(&b).ok() } else { worldextra::nav_segments(&b).ok() }).and_then(|segs| factory.line_part(&segs, colour, &format!("line:{kind}"))).map(|part| GpuModel { parts: vec![part] });
                let _ = tx.send(((kind, cell), model));
            });
        }
        None
    }

    /// Whether the model's pictures went up smaller than they could (the memory budget was filling).
    pub fn is_reduced(&self, hash: u32) -> bool {
        self.reduced.contains(&hash)
    }

    /// Load a reduced model again with its pictures at full size (the old one stays drawn until the new one arrives), when memory has room.
    pub fn upgrade(&mut self, hash: u32) {
        let k = (hash, false);
        if self.pending.contains(&k) || self.pending.len() >= MAX_PENDING || self.factory.texture_bytes() >= texture_budget() * 7 / 10 {
            return;
        }
        if dbg_env().texlog {
            eprintln!("upgrade {:?} ({} B on the GPU)", self.shared.names.get(&hash), self.factory.texture_bytes());
        }
        self.upgrading.lock().unwrap().insert(hash);
        self.wanted.lock().unwrap().insert(k, self.pass.load(std::sync::atomic::Ordering::Relaxed));
        self.pending.insert(k);
        let _ = self.tx.send(k);
    }

    /// models whose pictures are smaller than they could be, and models loaded again at full size
    pub fn upgrade_stats(&self) -> (usize, usize) {
        (self.reduced.len(), self.upgraded)
    }

    pub fn get(&self, hash: u32) -> Option<&Arc<GpuModel>> {
        self.models.get(&(hash, false)).and_then(|m| m.as_ref())
    }

    pub fn get_collision(&self, hash: u32) -> Option<&Arc<GpuModel>> {
        self.models.get(&(hash, true)).and_then(|m| m.as_ref())
    }

    /// whether the answer for this model is known (it may be "none")
    pub fn known(&self, hash: u32, coll: bool) -> bool {
        self.models.contains_key(&(hash, coll))
    }

    /// Make the user's model and texture files known (`.ydr .yft .ydd .ytd`), and ask again for models that were not found before.
    pub fn add_loose(&mut self, files: Vec<std::path::PathBuf>) -> usize {
        let mut n = 0;
        if dbg_env().debug {
            eprintln!("add_loose {} files", files.len());
        }
        for f in files {
            let (Some(name), Some(stem)) = (f.file_name().map(|s| s.to_string_lossy().to_lowercase()), f.file_stem().map(|s| s.to_string_lossy().to_lowercase())) else { continue };
            if [".ydr", ".yft", ".ydd", ".ytd"].iter().any(|e| name.ends_with(e)) {
                self.shared.loose_names.write().unwrap().insert(rage_toolkit_core::rage_joaat(&stem), stem);
                self.shared.loose.write().unwrap().insert(name, f);
                n += 1;
            }
        }
        // models that were looked for and not found may be there now
        self.models.retain(|_, m| m.is_some());
        n
    }

    /// A new picture is about to be made: requests that no recent picture asks for again are stale.
    pub fn new_pass(&mut self) {
        self.pass.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// how many answers have come in (including requests dropped as stale)
    pub fn arrivals(&self) -> usize {
        self.arrivals
    }

    pub fn loaded(&self) -> usize {
        self.models.values().filter(|m| m.is_some()).count() + self.lines.values().filter(|m| m.is_some()).count() + self.skeletons.len()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// The bones of an archetype's model, found the way [`load_model`] finds the model.
fn skeleton_of(s: &Shared, hash: u32) -> Option<Vec<([f32; 3], [f32; 3])>> {
    let name = |h: u32| s.names.get(&h).filter(|n| !n.starts_with("hash_")).map(|n| n.to_lowercase()).or_else(|| s.loose_names.read().unwrap().get(&h).cloned());
    let arch = s.data.arch_of(&hash);
    let asset = arch.and_then(|a| if a.asset_hash != 0 { name(a.asset_hash) } else { None }).or_else(|| name(hash))?;
    let candidates: Vec<(String, &str)> = match arch {
        Some(a) if a.asset_type == 3 && a.ydd_hash != 0 => name(a.ydd_hash).map(|d| (format!("{d}.ydd"), "ydd")).into_iter().collect(),
        Some(a) if a.asset_type == 1 => vec![(format!("{asset}.yft"), "yft")],
        _ => vec![(format!("{asset}.ydr"), "ydr"), (format!("{asset}.yft"), "yft")],
    };
    let (file, ext) = candidates.iter().find(|(f, _)| s.has_file(f)).map(|(f, e)| (f.clone(), *e))?;
    let bytes = s.read_named(&file)?;
    Some(worldmodel::skeleton_segments(&bytes, ext, &asset))
}

/// The collision of an archetype: its `.ybn` (named after the archetype or its asset), as coloured triangles.
fn load_collision(s: &Shared, factory: &Factory, hash: u32) -> Option<GpuModel> {
    let name = |h: u32| s.names.get(&h).filter(|n| !n.starts_with("hash_")).map(|n| n.to_lowercase());
    let arch = s.data.arch.get(&hash);
    let mut names: Vec<String> = vec![];
    names.extend(name(hash));
    names.extend(arch.and_then(|a| name(a.asset_hash)));
    // a world collision file (asked for by the hash of its name), or one named after the archetype
    let found = s.ybn_by_hash.get(&hash).or_else(|| names.iter().find_map(|n| s.files.get(&format!("{n}.ybn"))));
    if dbg_env().debug {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_coll.txt")) {
            let _ = writeln!(f, "{hash:08x} {:?} (names {names:?}, {} ybn known)", found, s.ybn_by_hash.len());
        }
    }
    let path = found?;
    let bytes = s.game.read(path).ok()?;
    let parts = worldextra::collision_parts(&bytes).ok()?;
    let mut gpu_parts = vec![];
    for p in &parts {
        let key = format!("col:{}", p.material);
        let [r, g, b] = worldextra::material_color(p.material);
        factory.add_texture(&key, 1, 1, &[r, g, b, 255]);
        if let Some(mut part) = factory.part(&p.positions, &p.normals, &[], &p.indices, &[], &[], Some(&key), (None, None), 0) {
            factory.add_wire(&mut part, &p.indices);
            gpu_parts.push(part);
        }
    }
    (!gpu_parts.is_empty()).then_some(GpuModel { parts: gpu_parts })
}

/// the model and whether any of its pictures went up smaller than they could; `full` loads every picture at full size (under keys of their own)
/// Why a model is missing, written to `%TEMP%\crck_world_absent.txt` (CRCK_WORLD_ABSENT=1): no name, no file, or a file that gave nothing to draw.
fn debug_absent(s: &Shared, hash: u32) {
    use std::io::Write;
    let name = |h: u32| s.names.get(&h).filter(|n| !n.starts_with("hash_")).map(|n| n.to_lowercase());
    let arch = s.data.arch_of(&hash);
    let asset = arch.and_then(|a| if a.asset_hash != 0 { name(a.asset_hash) } else { None }).or_else(|| name(hash));
    let why = match (&asset, arch) {
        (None, _) => "no-name".to_string(),
        (Some(a), arch) => {
            let kind = arch.map_or(0, |x| x.asset_type);
            let files: Vec<String> = match arch {
                Some(x) if x.asset_type == 3 && x.ydd_hash != 0 => name(x.ydd_hash).map(|d| format!("{d}.ydd")).into_iter().collect(),
                Some(x) if x.asset_type == 1 => vec![format!("{a}.yft")],
                _ => vec![format!("{a}.ydr"), format!("{a}.yft")],
            };
            match files.iter().find(|f| s.has_file(f)) {
                None => format!("no-file asset_type={kind} wanted={files:?}"),
                Some(f) => format!("file-gave-nothing {f}"),
            }
        }
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_absent.txt")) {
        let _ = writeln!(f, "{hash:08x}	{}	{why}", asset.unwrap_or_default());
    }
}

fn load_model(s: &Shared, factory: &Factory, ytds: &Mutex<VecDeque<(String, Arc<Ytd>)>>, hash: u32, full: bool) -> Option<(GpuModel, bool)> {
    let sfx = if full { "|full" } else { "" };
    let any_reduced = std::cell::Cell::new(false);
    if dbg_env().debug && hash == rage_toolkit_core::rage_joaat("my_custom_prop") {
        eprintln!("load_model custom requested; loose names {:?}", s.loose_names.read().unwrap().len());
    }
    let name = |h: u32| s.names.get(&h).filter(|n| !n.starts_with("hash_")).map(|n| n.to_lowercase()).or_else(|| s.loose_names.read().unwrap().get(&h).cloned());
    let arch = s.data.arch_of(&hash);
    // where the model is: a drawable or fragment by its asset name, or inside the dictionary that holds it
    // a drawable inside a dictionary (type 3) need not have a name the table knows: the dictionary's name finds the file, the hash picks the drawable
    let in_dict = arch.map_or(false, |a| a.asset_type == 3 && a.ydd_hash != 0 && name(a.ydd_hash).is_some());
    let asset = arch.and_then(|a| if a.asset_hash != 0 { name(a.asset_hash) } else { None }).or_else(|| name(hash)).or_else(|| if in_dict { Some(format!("hash_{:08x}", arch.map_or(hash, |a| a.asset_hash))) } else { None })?;
    let want_hash = if in_dict { arch.map_or(0, |a| a.asset_hash) } else { 0 };
    let candidates: Vec<(String, &str)> = match arch {
        Some(a) if a.asset_type == 3 && a.ydd_hash != 0 => name(a.ydd_hash).map(|d| (format!("{d}.ydd"), "ydd")).into_iter().collect(),
        Some(a) if a.asset_type == 1 => vec![(format!("{asset}.yft"), "yft")],
        _ => vec![(format!("{asset}.ydr"), "ydr"), (format!("{asset}.yft"), "yft")],
    };
    if dbg_env().debug && asset.contains("custom") {
        eprintln!("load_model {asset}: candidates {candidates:?}, loose {:?}", s.loose.read().unwrap().keys().collect::<Vec<_>>());
    }
    let (file, ext) = candidates.iter().find(|(f, _)| s.has_file(f)).map(|(f, e)| (f.clone(), *e))?;
    let bytes = s.read_named(&file)?;
    let (parts, embedded) = match worldmodel::parts_with_embedded_hash(&bytes, ext, &asset, want_hash) {
        Ok(p) => p,
        Err(e) => {
            if dbg_env().debug {
                eprintln!("parse {asset}: {e:#}");
            }
            return None;
        }
    };
    if parts.is_empty() {
        return None;
    }
    let txd = arch.and_then(|a| name(a.txd_hash)).unwrap_or_else(|| asset.clone());
    let chain = txd_chain(&s.parents, &txd);
    let mut gpu_parts = vec![];
    if dbg_env().masks {
        let mut m: Vec<u16> = parts.iter().map(|p| p.mask).collect();
        m.sort();
        m.dedup();
        eprintln!("MASKS {asset} {:04x?} ({} parts)", m, parts.len());
    }
    for p in &parts {
        // a proxy (a model for reflections or shadows) is not drawn as a surface; CRCK_WORLD_NOPROXYFILL drops it altogether. Kept as a
        // pushed-back fill (blend class 5): it only shows where nothing real is drawn (LOD gaps), never on top of real surfaces
        if p.proxy && dbg_env().no_proxy_fill {
            continue;
        }
        let find = |tex: &String| -> Option<String> {
            texture_names(&tex.to_lowercase()).iter().find_map(|n| {
                // the HD dictionary bound to the asset or its dictionaries first (even over a picture the model carries: the road models carry
                // small ones and a `+hidr` dictionary the large ones), then the model's own textures, then its dictionary chain, then the whole game
                if let Some(k) = hd_one(s, factory, ytds, &asset, n, full).or_else(|| chain.iter().find_map(|d| hd_one(s, factory, ytds, d, n, full))) {
                    return Some(k);
                }
                let ekey = format!("emb:{asset}|{n}{sfx}");
                if factory.has_texture(&ekey) {
                    return Some(ekey);
                }
                if embedded.has(n) {
                    if upload(factory, &embedded, n, &ekey, full)? {
                        any_reduced.set(true);
                    }
                    return Some(ekey);
                }
                let k = find_texture(s, factory, ytds, &chain, &asset, n, full)?;
                if factory.is_reduced(&k) {
                    any_reduced.set(true);
                }
                Some(k)
            })
        };
        let key = p.diffuse.as_ref().and_then(&find);
        if let Some(w) = dbg_env().tex_where.as_deref() {
            if asset.contains(&w) {
                eprintln!("TEXWHERE {asset} (txd {txd}, chain {chain:?}): {} -> {:?} [{}]", p.diffuse.as_deref().unwrap_or("-"), key, p.shader);
            }
        }
        // the normal and specular maps, when there are textures left to upload
        let (bump_key, spec_key) = (p.bump.as_ref().and_then(&find), p.spec.as_ref().and_then(&find));
        // a decal (road marking, dirt, leaf litter) with no picture would be a flat grey sheet over the ground: leave it out
        // and the same for any part whose picture cannot be found: only textured geometry is drawn, never grey stand-ins
        if dbg_env().debug {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_parts.txt")) {
                let _ = writeln!(f, "{}	{}	{}	{}	{}", p.shader, p.diffuse.as_deref().unwrap_or("-"), if key.is_some() { "texture" } else { "NONE" }, asset, p.indices.len() / 3);
            }
        }
        if key.is_none() {
            continue;
        }
        // terrain: four layers and a mask, when all of them are found
        if let (Some(t), Some(k0), false) = (&p.terrain, key.as_deref(), dbg_env().no_terrain) {
            let found: Vec<Option<String>> = t.iter().map(|n| find(n)).collect();
            if let [Some(l1), Some(l2), Some(l3), Some(m)] = found.as_slice() {
                if let Some(part) = factory.terrain_part(&p.positions, &p.normals, &p.uvs, &p.uvs1, &p.indices, [k0, l1, l2, l3, m]) {
                    gpu_parts.push(part);
                    continue;
                }
            }
        }
        // a proxy's vertex colours are not ambient occlusion (they would make it black where it faces away from the sun)
        let no_colours: &[[u8; 4]] = &[];
        if let Some(part) = factory.part(&p.positions, &p.normals, &p.uvs, &p.indices, if p.proxy { no_colours } else { &p.colors }, &p.tangents, key.as_deref(), (bump_key.as_deref(), spec_key.as_deref()), if p.proxy { 5 } else { blend_class(&p.shader).max(key.as_deref().map_or(0, |k| factory.is_soft(k) as u8)) }) {
            gpu_parts.push(part);
        }
    }
    (!gpu_parts.is_empty()).then_some((GpuModel { parts: gpu_parts }, any_reduced.get()))
}

/// The names a part's texture may be stored under. The model's string often holds the texture name and the name of its alpha picture
/// run together (`bj_weeds01` + `bj_weeds01_a`): then the first half is the texture.
fn texture_names(name: &str) -> Vec<String> {
    let mut v = vec![name.to_string()];
    for k in (3..=name.len() / 2).rev() {
        if name.is_char_boundary(k) && name[k..].starts_with(&name[..k]) {
            v.push(name[..k].to_string());
            break;
        }
    }
    v
}

/// `tex` from the HD dictionary the manifests bind to `name` (an asset or a texture dictionary), uploaded once; None when nothing is bound or the
/// dictionary has no such picture.
fn hd_one(s: &Shared, factory: &Factory, ytds: &Mutex<VecDeque<(String, Arc<Ytd>)>>, name: &str, tex: &str, full: bool) -> Option<String> {
    let sfx = if full { "|full" } else { "" };
    let hd = s.hd_bind.get(name)?;
    let key = format!("{hd}|{tex}{sfx}");
    if factory.has_texture(&key) {
        return Some(key);
    }
    let file = format!("{hd}.ytd");
    if !s.has_file(&file) {
        return None;
    }
    let ytd = cached_ytd(s, ytds, hd, &file)?;
    if !ytd.has(tex) || factory.texture_count() >= MAX_TEXTURES {
        return None;
    }
    upload(factory, &ytd, tex, &key, full)?;
    HD_TEXTURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(key)
}

/// Find `tex` in the dictionaries of `chain` (the model's own dictionary, then its parents), then in whichever dictionary of the whole game
/// has it. Upload it once and return its key.
fn find_texture(s: &Shared, factory: &Factory, ytds: &Mutex<VecDeque<(String, Arc<Ytd>)>>, chain: &[String], asset: &str, tex: &str, full: bool) -> Option<String> {
    let sfx = if full { "|full" } else { "" };
    // a texture of the HD dictionary bound to the asset or to a dictionary of the chain replaces the standard one
    let hd = |dict: &str| hd_one(s, factory, ytds, dict, tex, full);
    let from = |dict: &str| -> Option<Option<String>> {
        if let Some(k) = hd(asset).or_else(|| hd(dict)) {
            return Some(Some(k));
        }
        let key = format!("{dict}|{tex}{sfx}");
        if factory.has_texture(&key) {
            return Some(Some(key));
        }
        if dbg_env().texlog && !s.hd_bind.contains_key(dict) && s.has_file(&format!("{dict}+hi.ytd")) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("crck_world_implicit.txt")) {
                let _ = writeln!(f, "{dict} {tex}");
            }
        }
        let file = format!("{dict}.ytd");
        if !s.has_file(&file) {
            return None;
        }
        let ytd = cached_ytd(s, ytds, dict, &file)?;
        if !ytd.has(tex) {
            return None;
        }
        if factory.texture_count() >= MAX_TEXTURES {
            return Some(None);
        }
        upload(factory, &ytd, tex, &key, full)?;
        Some(Some(key))
    };
    for dict in chain {
        if let Some(r) = from(dict) {
            return r;
        }
    }
    s.tex_index.get(tex).and_then(|dict| from(dict)).flatten()
}

/// The last few parsed dictionaries (parsing keeps the compressed pixels, so only a handful are kept).
fn cached_ytd(s: &Shared, cache: &Mutex<VecDeque<(String, Arc<Ytd>)>>, dict: &str, file: &str) -> Option<Arc<Ytd>> {
    if let Some((_, y)) = cache.lock().unwrap().iter().find(|(k, _)| k == dict) {
        return Some(y.clone());
    }
    let ytd = Arc::new(Ytd::parse(&s.read_named(file)?).ok()?);
    let mut c = cache.lock().unwrap();
    c.push_back((dict.to_string(), ytd.clone()));
    while c.len() > 24 {
        c.pop_front();
    }
    Some(ytd)
}

/// How a shader's parts are drawn: decals (`decal*`) and translucent ones (glass, `*alpha*`) are blended by their texture's alpha after the
/// opaque parts, as the game does; cut-out shaders (`*cutout*`, alpha test) stay in the opaque pass with their transparent texels discarded.
fn blend_class(shader: &str) -> u8 {
    let s = shader.to_lowercase();
    // the cut-out shaders, the trees and the cloth are drawn with both sides of their triangles; every other
    // opaque surface only its front
    if s.contains("cutout") || s.contains("alphatest") || s.starts_with("trees") || s.contains("cloth") {
        6
    } else if s.contains("decal") {
        1
    } else if s.contains("glass") || s.contains("alpha") {
        2
    } else {
        0
    }
}

