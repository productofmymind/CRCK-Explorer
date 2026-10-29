//! CRCK Explorer viewer: open a .ydr/.ydd/.yft (and .ytd texture dictionaries), orbit it with its textures, inspect parts and textures.
//! All Rust: egui for the interface, wgpu for the 3D view (see gpu.rs). Parsing is rage-toolkit-core's.
// A window program: no console window of its own when double-clicked (debug builds keep one for the log output).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod browser;
mod camera;
mod dragout;
mod explorer;
mod gpu;
mod home;
mod preview;
mod settings;
mod settings_page;
mod theme;
mod text_window;
mod tools;
mod ytd_window;
mod ytyp_window;

use anyhow::Result;
use eframe::egui;
use glam::Vec3;
use browser::{Action as BAction, Kind as BKind, Loc};
use camera::Camera;
use gpu::Gpu;
use rage_toolkit_core::model::{self, LodLevel, MeshPart};
use std::path::{Path, PathBuf};

const MODEL_EXT: [&str; 3] = ["ydr", "ydd", "yft"];

/// the answer of a texture lookup in the game: dictionaries found, names nobody has
type TextureLookup = Result<(Vec<PathBuf>, Vec<String>)>;

/// A vehicle's files read from the game on a thread: the files to open in any case (a model from the Explorer and the
/// dictionaries next to it; empty for *Vehicle from game*), then what the game gave (or why it could not).
type VehicleFiles = (Vec<PathBuf>, Result<Vec<PathBuf>>);

struct PartRow {
    name: String,
    shader: String,
    diffuse: Option<String>,
    triangles: usize,
    vertices: usize,
    visible: bool,
    texture_found: bool,
    /// the diffuse name the model really asks for, when another variant of it was loaded and is shown instead
    asked: Option<String>,
    /// rotor blur plane: a huge flat quad that only looks right with its alpha texture, so it starts hidden
    blur: bool,
    min: Vec3,
    max: Vec3,
}

/// Ped textures come in variants (`hand_diff_000_a_uni`, `hand_diff_000_b_uni`): the model names one letter and the game
/// picks the letter, so when only another variant is loaded it is the one to show. Same name minus its single-letter
/// tokens = same stem.
fn variant_stem(name: &str) -> String {
    name.to_lowercase().split('_').filter(|t| !(t.len() == 1 && t.chars().all(|c| c.is_ascii_alphabetic()))).collect::<Vec<_>>().join("_")
}

fn variant_fallback<'a>(wanted: &str, loaded: &'a [String]) -> Option<&'a String> {
    let (w, stem) = (wanted.to_lowercase(), variant_stem(wanted));
    loaded.iter().find(|n| n.to_lowercase() != w && variant_stem(n) == stem)
}

/// Points every part whose diffuse is not loaded at a loaded variant of it, if there is one. Returns, per part, the name
/// the model really asked for when that happened.
fn apply_variant_fallback(meshes: &mut [MeshPart], loaded: &[String]) -> Vec<Option<String>> {
    let have: std::collections::HashSet<String> = loaded.iter().map(|n| n.to_lowercase()).collect();
    meshes
        .iter_mut()
        .map(|m| {
            let d = m.diffuse.clone().filter(|d| !have.contains(&d.to_lowercase()))?;
            let alt = variant_fallback(&d, loaded)?.clone();
            m.diffuse = Some(alt);
            Some(d)
        })
        .collect()
}

/// How a part is called in the Parts tab: its bone for a fragment (chassis, wheel_lf, ...), else the drawable's name.
fn part_label(m: &MeshPart) -> String {
    match (m.part == m.drawable, m.model) {
        (true, _) => format!("{} m{} g{}", m.drawable, m.model, m.geometry),
        // a bone name (bodyshell, windscreen, chassis 2, ...); repeated ones are numbered by the core
        (false, _) => m.part.clone(),
    }
}

/// Helicopter/plane rotor blur planes use `*blades*` textures with alpha; untextured they are opaque slabs.
fn is_blur_plane(m: &MeshPart) -> bool {
    m.shader.to_lowercase().starts_with("vehicle_blurredrotor") || m.diffuse.as_deref().map(|d| d.to_lowercase().contains("blade")).unwrap_or(false)
}

fn bounds_of(m: &MeshPart) -> (Vec3, Vec3) {
    let (mut mn, mut mx) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for p in &m.positions {
        let v = Vec3::from(*p);
        if v.is_finite() {
            mn = mn.min(v);
            mx = mx.max(v);
        }
    }
    (mn, mx)
}

struct TexRow {
    name: String,
    width: u32,
    height: u32,
    format: String,
    handle: egui::TextureHandle,
    /// the .ytd this texture lives in; None = embedded in the model (not editable)
    source: Option<PathBuf>,
    /// how many parts of the loaded model use it (diffuse, normal or specular)
    used: usize,
}

/// What the window shows: the start screen, the file explorer, the model viewer or the tools.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Home,
    Explorer,
    Viewer,
    Tools,
    Settings,
}

struct App {
    mode: Mode,
    /// open on the Home screen when CRCK Explorer starts (remembered between runs)
    show_home: bool,
    home_logo: Option<egui::TextureHandle>,
    theme_page: settings_page::Page,
    /// search boxes of the parts list and the texture list
    part_filter: String,
    tex_filter: String,
    explorer: explorer::Explorer,
    /// floating windows showing pictures opened from the Explorer
    previews: preview::Previews,
    /// floating windows listing the textures of a .ytd opened from the Explorer
    ytd_windows: ytd_window::Ytds,
    /// the app's window was closed with texture dictionaries not saved: asking what to do
    ask_quit: bool,
    /// floating editors for text files opened from the Explorer
    text_windows: text_window::Texts,
    /// the archetype editor windows
    ytyp_windows: ytyp_window::Ytyps,
    /// the Viewer was left with the back button or Alt+Left: forward comes back to it
    viewer_forward: bool,
    tools: tools::Tools,
    gpu: Option<Gpu>,
    model: Option<PathBuf>,
    ytds: Vec<PathBuf>,
    lod: LodLevel,
    parts: Vec<PartRow>,
    textures: Vec<TexRow>,
    selected_texture: Option<usize>,
    cam: Camera,
    textured: bool,
    show_grid: bool,
    status: String,
    bounds: (Vec3, Vec3),
    needs_reload: bool,
    needs_frame: bool,
    /// after a model opens, keep it fitted while the view changes shape (panels settling, window resizing) until the camera is moved by hand
    auto_frame: bool,
    framed_aspect: f32,
    pending_meshes: Option<Vec<MeshPart>>,
    pending_textures: Option<Vec<(String, u32, u32, Vec<u8>)>>,
    /// `--ui-screenshot out.png`: save the whole window after a few frames, then quit (automated checks)
    ui_shot: Option<PathBuf>,
    /// `--ytd-window` / `--picture` (automated screenshots): floating windows to open on the first frame
    shot_windows: Vec<(u8, PathBuf)>,
    frames: u32,
    show_game: bool,
    game_name: String,
    /// vehicle body colour for paint shaders
    paint: [f32; 3],
    /// normal maps, specular maps and reflections
    effects: bool,
    game: Option<rage_toolkit_core::game::Game>,
    /// texture names the loaded model asks for that nothing loaded has (lowercase)
    missing: Vec<String>,
    /// looking the missing textures up in the game's vehicle dictionaries (a thread: the first time reads them all)
    texture_job: Option<std::sync::mpsc::Receiver<TextureLookup>>,
    /// the model and its textures being loaded on a thread
    load_job: Option<std::sync::mpsc::Receiver<Result<Loaded>>>,
    /// a vehicle's files being read from the game on a thread
    vehicle_job: Option<std::sync::mpsc::Receiver<VehicleFiles>>,
    /// editor copies of files from editable archives (Edit mode): when one changes on disk it is written back into its archive
    write_back: Vec<WriteBack>,
    write_check: std::time::Instant,
    /// the window's own handle (Windows), for dragging files out of it
    hwnd: Option<isize>,
    /// the GPU the window draws with, for other 3D views (the clothing preview)
    render: Option<eframe::egui_wgpu::RenderState>,
    keep_format: bool,
    keep_size: bool,
    /// width / height of the 3D view the last time it was drawn
    aspect: f32,
}

/// An editor's copy of a file inside an editable archive, and when it was last written back.
struct WriteBack {
    file: PathBuf,
    place: explorer::Place,
    stamp: Option<std::time::SystemTime>,
}

fn modified(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// What a load makes (on a thread); the window takes it over when it is done.
struct Loaded {
    parts: Vec<PartRow>,
    textures: Vec<TexRow>,
    missing: Vec<String>,
    bounds: (Vec3, Vec3),
    status: String,
    meshes: Vec<MeshPart>,
    /// only the textures the model's parts use: the others are never drawn in 3D
    gpu_tex: Vec<(String, u32, u32, Vec<u8>)>,
}

/// Longest side of a picture in the Textures panel (a 56 px thumbnail, or the panel's width when enlarged). Bigger
/// textures are shown from a halved copy, so the panel does not hold a second full-size copy of every texture.
const PANEL_MAX: u32 = 512;

/// Parse the model and the texture dictionaries, decode the textures, make the Textures panel's pictures. Runs on a thread
/// (egui's `load_texture` may be called from any thread).
fn load_scene(ctx: &egui::Context, model_path: Option<PathBuf>, ytds: Vec<PathBuf>, lod: LodLevel, old_bounds: (Vec3, Vec3)) -> Result<Loaded> {
    let t0 = std::time::Instant::now();
    let mut meshes = match &model_path {
        Some(p) => model::placed_meshes(p, lod)?,
        None => vec![],
    };
    let raw = model::load_textures_sourced(model_path.as_deref(), &ytds)?;
    let mut seen = std::collections::HashSet::new();
    let mut gpu_tex = vec![];
    let mut rows = vec![];
    for (t, source) in &raw {
        if !seen.insert(t.name.to_lowercase()) {
            continue;
        }
        if let Ok((w, h, rgba)) = model::texture_rgba(t) {
            // the panel's picture: a copy only when the texture is bigger than the panel needs
            let img = if w.max(h) > PANEL_MAX {
                let (hw, hh, halved) = gpu::half(w, h, &rgba);
                let (pw, ph, small) = gpu::shrink_to(hw, hh, halved, PANEL_MAX);
                egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &small)
            } else {
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba)
            };
            let handle = ctx.load_texture(format!("tex_{}_{}", t.name, rows.len()), img, egui::TextureOptions::LINEAR);
            rows.push(TexRow { name: t.name.clone(), width: w, height: h, format: format!("{:?}", t.format), handle, source: source.clone(), used: 0 });
            gpu_tex.push((t.name.clone(), w, h, rgba));
        }
    }
    let have: std::collections::HashSet<String> = gpu_tex.iter().map(|t| t.0.to_lowercase()).collect();
    let loaded: Vec<String> = gpu_tex.iter().map(|t| t.0.clone()).collect();
    let asked = apply_variant_fallback(&mut meshes, &loaded);
    // what the model still asks for (diffuse, normal and specular maps) that nothing loaded has
    let mut missing: Vec<String> = vec![];
    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for m in &meshes {
        let mut names: Vec<String> = [&m.diffuse, &m.bump, &m.spec].into_iter().flatten().map(|t| t.to_lowercase()).collect();
        names.sort();
        names.dedup();
        for t in names {
            if !have.contains(&t) && !missing.contains(&t) {
                missing.push(t.clone());
            }
            *used.entry(t).or_default() += 1;
        }
    }
    let (mut mn, mut mx) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for m in &meshes {
        let (a, b) = bounds_of(m);
        mn = mn.min(a);
        mx = mx.max(b);
    }
    if !mn.is_finite() {
        if let Some(p) = &model_path {
            anyhow::bail!("{} has no drawable geometry at the {} LOD", p.display(), lod_name(lod));
        }
        (mn, mx) = old_bounds;
    }
    let label = model_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "texture dictionary".into());
    let parts: Vec<PartRow> = meshes
        .iter()
        .zip(asked)
        .map(|(m, asked)| {
            let blur = is_blur_plane(m);
            let (min, max) = bounds_of(m);
            PartRow {
                name: part_label(m),
                shader: m.shader.clone(),
                diffuse: m.diffuse.clone(),
                triangles: m.indices.len() / 3,
                vertices: m.positions.len(),
                visible: !blur,
                texture_found: m.diffuse.as_ref().map(|d| have.contains(&d.to_lowercase())).unwrap_or(false),
                asked,
                blur,
                min,
                max,
            }
        })
        .collect();
    let tris: usize = parts.iter().map(|p| p.triangles).sum();
    let status = format!(
        "{} - {} part(s), {tris} triangles, {} texture(s) loaded in {:.0} ms{}",
        label,
        parts.len(),
        rows.len(),
        t0.elapsed().as_secs_f64() * 1000.0,
        if missing.is_empty() { String::new() } else { format!("; {} texture(s) the model names are not loaded", missing.len()) }
    );
    for r in &mut rows {
        r.used = used.get(&r.name.to_lowercase()).copied().unwrap_or(0);
    }
    gpu_tex.retain(|t| used.contains_key(&t.0.to_lowercase()));
    Ok(Loaded { parts, textures: rows, missing, bounds: (mn, mx), status, meshes, gpu_tex })
}

fn lod_name(l: LodLevel) -> &'static str {
    match l {
        LodLevel::High => "High",
        LodLevel::Medium => "Medium",
        LodLevel::Low => "Low",
        LodLevel::VeryLow => "Very low",
    }
}

impl App {
    fn new(args: Vec<PathBuf>) -> Self {
        let game = rage_toolkit_core::game::Game::open(None).ok();
        let explorer = explorer::Explorer::new(game.as_ref());
        let mut a = Self {
            mode: Mode::Viewer,
            show_home: settings::show_home(&settings::path()),
            home_logo: None,
            theme_page: settings_page::Page::new(),
            part_filter: String::new(),
            tex_filter: String::new(),
            explorer,
            previews: Default::default(),
            ytd_windows: Default::default(),
            ask_quit: false,
            text_windows: Default::default(),
            ytyp_windows: Default::default(),
            viewer_forward: false,
            tools: tools::Tools::new(),
            gpu: None,
            model: None,
            ytds: vec![],
            lod: LodLevel::High,
            parts: vec![],
            textures: vec![],
            selected_texture: None,
            cam: Camera::new(),
            textured: true,
            show_grid: true,
            status: "Ready. Open a model, or drop files on the window.".into(),
            bounds: (Vec3::splat(-1.0), Vec3::splat(1.0)),
            needs_reload: false,
            needs_frame: false,
            auto_frame: false,
            framed_aspect: 0.0,
            pending_meshes: None,
            pending_textures: None,
            ui_shot: None,
            shot_windows: vec![],
            frames: 0,
            show_game: false,
            game_name: String::new(),
            paint: [0.62, 0.07, 0.07],
            effects: true,
            game,
            missing: vec![],
            texture_job: None,
            load_job: None,
            vehicle_job: None,
            write_back: vec![],
            write_check: std::time::Instant::now(),
            hwnd: None,
            render: None,
            keep_format: true,
            keep_size: false,
            aspect: 1.6,
        };
        a.accept(args);
        a
    }

    /// models set the model, .ytd files add texture dictionaries
    fn accept(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if MODEL_EXT.contains(&ext.as_str()) {
                // a texture dictionary named like the model, in the same folder, comes along
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                for t in browser::sibling_textures(&Loc::Disk(p.clone()), &name) {
                    if !self.ytds.contains(&t) {
                        self.ytds.push(t);
                    }
                }
                self.model = Some(p);
                self.needs_reload = true;
                self.needs_frame = true;
                self.auto_frame = true;
            } else if ext == "ytd" {
                if !self.ytds.contains(&p) {
                    self.ytds.push(p);
                }
                self.needs_reload = true;
            } else {
                self.status = format!("Ignored {} (not a model or .ytd)", p.display());
            }
        }
    }

    /// Load the model and its texture dictionaries on a thread, so the window keeps drawing (a vehicle with the shared
    /// dictionaries is about 80 textures and 100 MB of pixels). A newer load replaces an older one still running.
    fn start_load(&mut self, ctx: &egui::Context) {
        if self.model.is_none() && self.ytds.is_empty() {
            return;
        }
        let (c, model_path, ytds, lod, bounds) = (ctx.clone(), self.model.clone(), self.ytds.clone(), self.lod, self.bounds);
        let label = model_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the textures".into());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_scene(&c, model_path, ytds, lod, bounds));
            c.request_repaint();
        });
        self.load_job = Some(rx);
        self.status = format!("Loading {label}...");
    }

    /// Take over what a finished load made.
    fn apply_loaded(&mut self, l: Loaded) {
        self.parts = l.parts;
        self.textures = l.textures;
        self.missing = l.missing;
        self.status = l.status;
        self.selected_texture = None;
        self.bounds = l.bounds;
        self.pending_meshes = Some(l.meshes);
        self.pending_textures = Some(l.gpu_tex);
        // framing waits for the 3D view to report its shape (a narrow window needs a larger distance)
    }

    fn frame_model(&mut self) {
        let vis: Vec<&PartRow> = self.parts.iter().filter(|p| p.visible && p.min.is_finite()).collect();
        let (mn, mx) = if vis.is_empty() {
            self.bounds
        } else {
            (vis.iter().fold(Vec3::splat(f32::INFINITY), |a, p| a.min(p.min)), vis.iter().fold(Vec3::splat(f32::NEG_INFINITY), |a, p| a.max(p.max)))
        };
        self.cam.target = (mn + mx) * 0.5;
        self.cam.dist = Camera::fit_dist(mn, mx, self.aspect);
    }

    /// Pull a vehicle (models, its textures, the shared vehshare dictionaries) out of the user's installed game and show it.
    fn load_vehicle_from_game(&mut self, name: &str) {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            return;
        }
        self.status = format!("Reading {name} from the game...");
        // walking the game's archives takes a second or more: on a thread, so the window keeps drawing
        let game = self.game.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = game.map(Ok).unwrap_or_else(|| rage_toolkit_core::game::Game::open(None)).and_then(|g| g.extract_vehicle(&name, &rage_toolkit_core::game::cache_dir().join("vehicles").join(&name)));
            // the base model (not _hi) is the default; the textures all come along
            let _ = tx.send((vec![], res.map(|files| files.into_iter().filter(|f| !f.to_string_lossy().to_lowercase().ends_with("_hi.yft")).collect())));
        });
        self.vehicle_job = Some(rx);
        self.mode = Mode::Viewer;
    }

    /// Open a model from the Explorer with the dictionaries the game chains to for it (vehicles.meta), read on a thread.
    fn load_with_game_textures(&mut self, files: Vec<PathBuf>, g: rage_toolkit_core::game::Game, stem: String) {
        self.status = format!("Reading the texture dictionaries of {stem} from the game...");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = g.vehicle_texture_files(&stem, &rage_toolkit_core::game::cache_dir().join("vehicles").join(&stem)).map_err(|e| e.context(format!("Texture dictionaries of {stem}")));
            let _ = tx.send((files, res));
        });
        self.vehicle_job = Some(rx);
        self.mode = Mode::Viewer;
    }

    /// Looks the textures the model names but nothing loaded has up in the game's vehicle dictionaries (on a thread: the
    /// first time it reads them all to build an index).
    fn find_missing_textures(&mut self) {
        let (Some(g), false, true) = (self.game.clone(), self.missing.is_empty(), self.texture_job.is_none()) else { return };
        let names = self.missing.clone();
        let stem = self.model.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_else(|| "model".into());
        let out = rage_toolkit_core::game::cache_dir().join("found_textures").join(stem);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(g.find_vehicle_textures(&names, &out));
        });
        self.texture_job = Some(rx);
        self.status = format!("Looking for {} texture(s) in the game's vehicle dictionaries (the first time reads them all)...", self.missing.len());
    }

    /// Picks up the jobs that have finished (vehicle files, loading, texture lookup), and keeps the window refreshing while they run.
    fn poll_jobs(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;
        if let Some(rx) = &self.vehicle_job {
            match rx.try_recv() {
                Ok((base, res)) => {
                    self.vehicle_job = None;
                    match res {
                        Ok(more) => {
                            self.ytds.clear();
                            self.accept(base.into_iter().chain(more).collect());
                        }
                        // a model from the Explorer still opens without the game's dictionaries
                        Err(e) if !base.is_empty() => {
                            self.ytds.clear();
                            self.accept(base);
                            self.status = format!("{e:#}");
                        }
                        Err(e) => self.status = format!("Game: {e:#}"),
                    }
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(TryRecvError::Disconnected) => self.vehicle_job = None,
            }
        }
        if let Some(rx) = &self.load_job {
            match rx.try_recv() {
                Ok(res) => {
                    self.load_job = None;
                    match res {
                        Ok(l) => self.apply_loaded(l),
                        Err(e) => self.status = format!("Error: {e:#}"),
                    }
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(TryRecvError::Disconnected) => self.load_job = None,
            }
        }
        let Some(rx) = &self.texture_job else { return };
        match rx.try_recv() {
            Ok(res) => {
                self.texture_job = None;
                match res {
                    Ok((files, nobody)) => {
                        let names: Vec<String> = files.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
                        for f in files {
                            if !self.ytds.contains(&f) {
                                self.ytds.push(f);
                            }
                        }
                        self.needs_reload = true;
                        self.status = format!(
                            "Added {}{}",
                            if names.is_empty() { "nothing".to_string() } else { names.join(", ") },
                            if nobody.is_empty() { String::new() } else { format!("; not in any vehicle dictionary of the game (script render targets and custom textures never are): {}", nobody.join(", ")) }
                        );
                    }
                    Err(e) => self.status = format!("Looking for textures failed: {e:#}"),
                }
            }
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(TryRecvError::Disconnected) => self.texture_job = None,
        }
    }

    fn full_to_loc(&self, full: &str) -> Result<(Loc, String)> {
        let g = self.game.as_ref().ok_or_else(|| anyhow::anyhow!("no game"))?;
        let norm = full.replace(char::from(92u8), "/");
        let i = norm.to_lowercase().rfind(".rpf/").ok_or_else(|| anyhow::anyhow!("not inside an archive"))?;
        let (arch, inner) = (&norm[..i + 4], &norm[i + 5..]);
        let ar = std::rc::Rc::new(g.archive(arch)?);
        let name = inner.rsplit('/').next().unwrap_or(inner).to_string();
        Ok((Loc::Arch { ar, path: inner.to_string(), origin: arch.to_string() }, name))
    }

    /// Is `p` one of the game's own files (in the game folder, not in the mods folder) while the Settings warning is on?
    fn warn_for_game_file(&self, p: &Path) -> Option<(PathBuf, PathBuf)> {
        let g = self.game.as_ref().filter(|_| self.theme_page.warn_game_files)?;
        let mods_dir = self.theme_page.mods_dir(Some(&g.dir))?;
        rage_toolkit_core::mods::game_relative(&g.dir, &mods_dir, p).map(|_| (g.dir.clone(), mods_dir))
    }

    /// A loose file of the game itself is about to be opened in an editor: ask first, like the classic tools' "mods" notice. The answer
    /// can be to copy it to the mods folder and edit the copy. None = cancelled.
    fn guard_game_file(&mut self, loc: Loc, name: &str) -> Option<Loc> {
        let Loc::Disk(p) = &loc else { return Some(loc) };
        let p = p.clone();
        let Some((game_dir, mods_dir)) = self.warn_for_game_file(&p) else { return Some(loc) };
        let (copy, edit) = ("Copy to \"mods\" and edit the copy", "Edit the game's own file");
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("One of the game's own files")
            .set_description(format!("{name} is part of GTA V itself:\n{}\n\nSaving would change the game's own file. Edit a copy in the \"mods\" folder instead?\n\nYes: copy it to \"mods\" and edit the copy.\nNo: edit the game's own file.\n\n(Settings > Game files can switch this question off.)", p.display()))
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(copy.into(), edit.into(), "Cancel".into()))
            .show();
        // Windows may show plain Yes / No / Cancel instead of the labels and answer Yes / No: both forms count
        let pick = match answer {
            rfd::MessageDialogResult::Custom(t) if t == copy => 1,
            rfd::MessageDialogResult::Custom(t) if t == edit => 2,
            rfd::MessageDialogResult::Yes => 1,
            rfd::MessageDialogResult::No => 2,
            _ => 0,
        };
        match pick {
            1 => match rage_toolkit_core::mods::copy_to_mods(&game_dir, &mods_dir, &p, &mut |_, _| true) {
                Ok(c) => {
                    self.status = format!("Editing the copy in the mods folder: {}", c.path().display());
                    Some(Loc::Disk(c.path().to_path_buf()))
                }
                Err(e) => {
                    self.status = format!("Copy to \"mods\" folder failed: {e:#}");
                    None
                }
            },
            2 => Some(loc),
            _ => None,
        }
    }

    /// Remember an editor's copy of a file from an editable archive (`place` None: nothing to remember).
    fn track(&mut self, file: &Path, place: Option<explorer::Place>) {
        let Some(place) = place else { return };
        self.write_back.retain(|w| w.file != file);
        self.write_back.push(WriteBack { file: file.to_path_buf(), place, stamp: modified(file) });
        self.status = format!("{} is saved back into the archive when you save it (Edit mode)", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }

    /// Twice a second: an editor copy that changed on disk is written back into its archive.
    fn check_write_back(&mut self, ctx: &egui::Context) {
        if self.write_back.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        if self.write_check.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.write_check = std::time::Instant::now();
        for i in 0..self.write_back.len() {
            let now = modified(&self.write_back[i].file);
            if now.is_none() || now == self.write_back[i].stamp {
                continue;
            }
            let Ok(data) = std::fs::read(&self.write_back[i].file) else { continue };
            let place = self.write_back[i].place.clone();
            // one edit at a time: when another is being written, this one waits for the next check
            if self.explorer.save_back(&place, data, self.game.as_ref()) {
                self.write_back[i].stamp = now;
            }
        }
    }

    fn handle_actions(&mut self, ctx: &egui::Context, actions: Vec<BAction>) {
        for a in actions {
            // a file opened from an editable archive in Edit mode: its editor saves back into the archive
            let place = self.explorer.opened_place.take();
            // the editors on one of the game's own loose files ask first (Settings can switch that off)
            let a = match a {
                BAction::Text(loc, n) => match self.guard_game_file(loc, &n) {
                    Some(l) => BAction::Text(l, n),
                    None => continue,
                },
                BAction::Ytd(loc, n) => match self.guard_game_file(loc, &n) {
                    Some(l) => BAction::Ytd(l, n),
                    None => continue,
                },
                BAction::Ytyp(loc, n) => match self.guard_game_file(loc, &n) {
                    Some(l) => BAction::Ytyp(l, n),
                    None => continue,
                },
                other => other,
            };
            match a {
                BAction::Settings => self.mode = Mode::Settings,
                BAction::DragOut(loc, name, kind) => {
                    // a real file (or folder) with the item's name: on disk as it is, from an archive a cached copy
                    let file = match (&loc, kind) {
                        (Loc::Disk(p), _) => Ok(p.clone()),
                        (Loc::Arch { .. }, BKind::Dir) => {
                            let dir = rage_toolkit_core::game::cache_dir().join("drag").join(format!("{}", std::process::id()));
                            let _ = std::fs::remove_dir_all(&dir);
                            std::fs::create_dir_all(&dir).map_err(anyhow::Error::from).and_then(|_| browser::extract(&loc, &name, kind, &dir, self.game.as_ref())).map(|_| dir.join(&name))
                        }
                        _ => browser::materialize(&loc, &name),
                    };
                    match (file, self.hwnd) {
                        (Ok(f), Some(h)) => {
                            if let Err(e) = dragout::drag_files(h, &[f]) {
                                self.explorer.message = format!("Drag: {e}");
                            }
                        }
                        (Err(e), _) => self.explorer.message = format!("{name}: {e:#}"),
                        (Ok(_), None) => {}
                    }
                }
                BAction::Text(loc, name) => match browser::materialize(&loc, &name) {
                    Ok(p) => {
                        match self.text_windows.open(&p, matches!(loc, Loc::Arch { .. }) && place.is_none()) {
                            Ok(()) => self.track(&p, place.clone()),
                            Err(e) => {
                                self.status = e.clone();
                                self.explorer.message = e;
                            }
                        }
                    }
                    Err(e) => self.status = format!("{name}: {e:#}"),
                },
                BAction::Ytyp(loc, name) => match browser::materialize(&loc, &name) {
                    Ok(p) => {
                        match self.ytyp_windows.open(&p, matches!(loc, Loc::Arch { .. }) && place.is_none()) {
                            Ok(()) => self.track(&p, place.clone()),
                            Err(e) => {
                                self.status = e.clone();
                                self.explorer.message = e;
                            }
                        }
                    }
                    Err(e) => self.status = format!("{name}: {e:#}"),
                },
                BAction::Ytd(loc, name) => match browser::materialize(&loc, &name) {
                    Ok(p) => {
                        match self.ytd_windows.open(ctx, &p, matches!(loc, Loc::Arch { .. }) && place.is_none()) {
                            Ok(()) => self.track(&p, place.clone()),
                            Err(e) => self.status = e,
                        }
                    }
                    Err(e) => self.status = format!("{name}: {e:#}"),
                },
                BAction::Preview(loc, name) => match browser::read(&loc) {
                    Ok(bytes) => {
                        if let Err(e) = self.previews.open(ctx, &name, &bytes, bytes.len() as u64) {
                            self.status = e;
                        }
                    }
                    Err(e) => self.status = format!("{name}: {e:#}"),
                },
                BAction::Open(loc, name) => self.open_loc(loc, name),
                BAction::AddTextures(loc, name) => match browser::materialize(&loc, &name) {
                    Ok(p) => self.accept(vec![p]),
                    Err(e) => self.status = format!("{e:#}"),
                },
                BAction::ExportXml(loc, name) => {
                    if let Some(d) = rfd::FileDialog::new().set_file_name(format!("{name}.xml")).save_file() {
                        self.status = match browser::read(&loc).and_then(|data| rage_toolkit_core::xmlconv::export(&name, &data)).and_then(|xml| Ok(std::fs::write(&d, xml)?)) {
                            Ok(()) => format!("{name} written as XML to {}", d.display()),
                            Err(e) => format!("{name}: XML export failed: {e:#}"),
                        };
                    }
                }
                BAction::Extract(loc, name, kind) => {
                    let dest = if kind == BKind::File { rfd::FileDialog::new().set_file_name(&name).save_file() } else { rfd::FileDialog::new().pick_folder() };
                    if let Some(d) = dest {
                        self.status = match browser::extract(&loc, &name, kind, &d, self.game.as_ref()) {
                            Ok(n) => format!("extracted {n} file(s) to {}", d.display()),
                            Err(e) => format!("extract failed: {e:#}"),
                        };
                    }
                }
            }
        }
    }

    /// double-click on a model or texture dictionary in the Explorer: look at it
    fn open_loc(&mut self, loc: Loc, name: String) {
        let ext = std::path::Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "yft" | "ydr" | "ydd" => match browser::materialize(&loc, &name) {
                Ok(model) => {
                    let mut files = vec![model];
                    files.extend(browser::sibling_textures(&loc, &name));
                    if ext == "yft" {
                        if let Some(g) = self.game.clone() {
                            // the dictionaries the game chains to for this vehicle (vehicles.meta), then the shared vehshare ones
                            let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                            let stem = stem.strip_suffix("_hi").unwrap_or(&stem).to_string();
                            self.load_with_game_textures(files, g, stem);
                            return;
                        }
                    }
                    self.ytds.clear();
                    self.accept(files);
                    self.mode = Mode::Viewer;
                }
                Err(e) => self.status = format!("{name}: {e:#}"),
            },
            "ytd" => match browser::materialize(&loc, &name) {
                Ok(p) => {
                    self.ytds.clear();
                    self.accept(vec![p]);
                    self.mode = Mode::Viewer;
                }
                Err(e) => self.status = format!("{name}: {e:#}"),
            },
            _ => self.status = format!("{name}: no viewer for this file type yet (right-click > Extract...)"),
        }
    }

    /// 0 = export DDS, 1 = export PNG, 2 = replace
    fn texture_action(&mut self, i: usize, action: u8) {
        let Some(t) = self.textures.get(i) else { return };
        let (name, source) = (t.name.clone(), t.source.clone());
        let Some(src_ytd) = source else {
            self.status = format!("{name} is embedded in the model, not in a .ytd: export/replace works on texture dictionaries");
            return;
        };
        match action {
            0 | 1 => {
                let (ext, label) = if action == 0 { ("dds", "DirectDraw Surface") } else { ("png", "PNG image") };
                if let Some(out) = rfd::FileDialog::new().set_file_name(format!("{name}.{ext}")).add_filter(label, &[ext]).save_file() {
                    self.status = match rage_toolkit_core::texture::export_texture(&src_ytd, &name, &out) {
                        Ok(()) => format!("exported {name} to {}", out.display()),
                        Err(e) => format!("export failed: {e:#}"),
                    };
                }
            }
            _ => {
                let Some(img) = rfd::FileDialog::new().set_title(format!("Replace {name} with...")).add_filter("DDS or image", &["dds", "png", "jpg", "jpeg", "tga", "bmp", "webp"]).pick_file() else { return };
                // files from the game are cached copies: never write over them, ask where to save instead; the same for one of
                // the game's own loose files while the Settings warning is on
                let cached = src_ytd.starts_with(rage_toolkit_core::game::cache_dir()) || self.warn_for_game_file(&src_ytd).is_some();
                let dest = if cached { rfd::FileDialog::new().set_file_name(src_ytd.file_name().unwrap().to_string_lossy().to_string()).add_filter("Texture dictionary", &["ytd"]).save_file() } else { Some(src_ytd.clone()) };
                let Some(dest) = dest else { return };
                if dest == src_ytd {
                    let bak = src_ytd.with_extension("ytd.bak");
                    if !bak.exists() {
                        let _ = std::fs::copy(&src_ytd, &bak);
                    }
                }
                let opts = rage_toolkit_core::texture::ReplaceOpts { keep_format: self.keep_format, keep_size: self.keep_size };
                match rage_toolkit_core::texture::replace_texture(&src_ytd, &name, &img, &dest, opts) {
                    Ok(msg) => {
                        for y in self.ytds.iter_mut().filter(|y| **y == src_ytd) {
                            *y = dest.clone();
                        }
                        self.needs_reload = true;
                        self.status = format!("replaced {msg}; saved to {}{}", dest.display(), if dest == src_ytd { " (backup: .ytd.bak)" } else { "" });
                    }
                    Err(e) => self.status = format!("replace failed: {e:#}"),
                }
            }
        }
    }

    fn ui_parts(&mut self, ui: &mut egui::Ui) {
        if self.parts.is_empty() {
            ui.label("No model loaded.");
            ui.weak("Open one with the button above, drop it on the window, or double-click it in the Explorer.");
            return;
        }
        let tris: usize = self.parts.iter().map(|p| p.triangles).sum();
        ui.label(format!("{} part(s), {tris} triangles. Tick the parts to show.", self.parts.len()));
        ui.horizontal(|ui| {
            if ui.small_button("Show all").clicked() {
                self.parts.iter_mut().for_each(|p| p.visible = true);
            }
            if ui.small_button("Hide all").clicked() {
                self.parts.iter_mut().for_each(|p| p.visible = false);
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.part_filter).hint_text("Search parts").desired_width(f32::INFINITY));
        let f = self.part_filter.trim().to_lowercase();
        let (ok, warn, dim) = (crate::theme::ok(), crate::theme::warn(), crate::theme::dim());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for p in &mut self.parts {
                if !f.is_empty() && !p.name.to_lowercase().contains(&f) && !p.shader.to_lowercase().contains(&f) {
                    continue;
                }
                let (tex, colour) = match (&p.diffuse, p.texture_found) {
                    (Some(d), true) if p.asked.is_some() => (format!("texture {d} (the model asks for {}; this variant is shown instead)", p.asked.as_deref().unwrap_or_default()), ok),
                    (Some(d), true) => (format!("texture {d}"), ok),
                    (Some(d), false) => (format!("texture {d} is not loaded: use Add textures..."), warn),
                    (None, _) => ("no diffuse texture".to_string(), dim),
                };
                let detail = format!("{} - {} tris, {} verts
{tex}{}", p.shader, p.triangles, p.vertices, if p.blur { "
rotor blur plane (hidden at first; needs its alpha texture)" } else { "" });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut p.visible, &p.name).on_hover_text(detail);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if colour == warn {
                            ui.label(egui::RichText::new("no texture").small().color(warn));
                        } else {
                            ui.label(egui::RichText::new(&p.shader).small().color(dim));
                        }
                    });
                });
            }
        });
    }

    fn open_model_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("GTA V models", &MODEL_EXT).pick_file() {
            self.accept(vec![p]);
        }
    }

    fn add_ytd_dialog(&mut self) {
        if let Some(ps) = rfd::FileDialog::new().add_filter("Texture dictionaries", &["ytd"]).pick_files() {
            self.accept(ps);
        }
    }
}

impl App {
    /// A card on the Home screen was clicked.
    fn go(&mut self, go: home::Go) {
        match go {
            home::Go::Explorer => self.mode = Mode::Explorer,
            home::Go::OpenModel => {
                self.mode = Mode::Viewer;
                self.open_model_dialog();
            }
            home::Go::VehicleFromGame => {
                self.mode = Mode::Viewer;
                self.show_game = true;
            }
            home::Go::Ytyp => {
                let ask = rfd::MessageDialog::new().set_title("Archetype editor").set_description("Open an existing .ytyp, or start a new empty one?\n\nYes: open a file.\nNo: a new empty file.").set_buttons(rfd::MessageButtons::YesNoCancelCustom("Open a file".into(), "New file".into(), "Cancel".into())).show();
                // Windows may show plain Yes / No / Cancel instead of the labels and answer Yes / No: both forms count
                let ask = match ask {
                    rfd::MessageDialogResult::Yes => rfd::MessageDialogResult::Custom("Open a file".into()),
                    rfd::MessageDialogResult::No => rfd::MessageDialogResult::Custom("New file".into()),
                    other => other,
                };
                match ask {
                    rfd::MessageDialogResult::Custom(t) if t == "Open a file" => {
                        if let Some(f) = rfd::FileDialog::new().add_filter("Archetype file", &["ytyp"]).pick_file() {
                            if let Err(e) = self.ytyp_windows.open(&f, false) {
                                self.status = e;
                            }
                        }
                    }
                    rfd::MessageDialogResult::Custom(t) if t == "New file" => self.ytyp_windows.open_new(),
                    _ => {}
                }
            }
            home::Go::Tool(p) => {
                self.mode = Mode::Tools;
                self.tools.page = p;
            }
        }
    }

    /// the viewer's buttons and view options (left to right: file actions; right side: view options)
    fn viewer_toolbar(&mut self, ui: &mut egui::Ui) {
                if ui.button("Open model...").on_hover_text("Open a .ydr, .ydd or .yft (Ctrl+O). Its matching .ytd next to it loads too. You can also drop files on the window.").clicked() {
                    self.open_model_dialog();
                }
                if ui.button("Vehicle from game...").on_hover_text("Load a vehicle by spawn name from your installed GTA V").clicked() {
                    self.show_game = true;
                }
                ui.separator();
                if ui.button("Add textures...").on_hover_text("Add .ytd texture dictionaries to the loaded textures").clicked() {
                    self.add_ytd_dialog();
                }
                if !self.ytds.is_empty() && ui.button("Clear textures").on_hover_text("Forget the loaded .ytd files").clicked() {
                    self.ytds.clear();
                    self.needs_reload = true;
                }
                // the view options sit on the right; added right to left, so in reverse
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Frame").on_hover_text("Fit the model in the view (F, or double-click the view)").clicked() {
                        self.frame_model();
                    }
                    ui.toggle_value(&mut self.show_grid, "Grid").on_hover_text("Ground grid (G)");
                    if self.parts.iter().any(|p| gpu::classify(&p.shader) == 1) {
                        ui.color_edit_button_rgb(&mut self.paint).on_hover_text("Body colour for vehicle paint");
                        ui.label("Paint");
                    }
                    ui.toggle_value(&mut self.effects, "Effects").on_hover_text("Normal maps, specular highlights and reflections (E)");
                    ui.toggle_value(&mut self.textured, "Textures").on_hover_text("Show the textures, or plain grey (T)");
                    let before = self.lod;
                    egui::ComboBox::from_id_salt("lod").selected_text(format!("Detail: {}", lod_name(self.lod))).show_ui(ui, |ui| {
                        for l in [LodLevel::High, LodLevel::Medium, LodLevel::Low, LodLevel::VeryLow] {
                            ui.selectable_value(&mut self.lod, l, lod_name(l));
                        }
                    });
                    if before != self.lod {
                        self.needs_reload = true;
                    }
                });
    }

    /// what ends every frame, whichever mode is showing: keep reloading, and the automated window screenshot
    /// Where the mouse is, in points. During a drag from Explorer no mouse moves reach the window, so on Windows the system
    /// is asked; elsewhere it is the last position egui saw.
    fn mouse_pos(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        #[cfg(windows)]
        if let Some(h) = self.hwnd {
            use windows_sys::Win32::{Foundation::POINT, Graphics::Gdi::ScreenToClient, UI::WindowsAndMessaging::GetCursorPos};
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: two plain Win32 calls on a point owned here and this app's own window handle
            if unsafe { GetCursorPos(&mut p) != 0 && ScreenToClient(h as *mut std::ffi::c_void, &mut p) != 0 } {
                let ppp = ctx.pixels_per_point();
                return Some(egui::pos2(p.x as f32 / ppp, p.y as f32 / ppp));
            }
        }
        ctx.input(|i| i.pointer.latest_pos())
    }

    /// Closing the app with texture dictionaries not saved asks first: save them, quit without saving, or stay.
    fn quit_question(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.ytd_windows.unsaved().is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ask_quit = true;
        }
        if !self.ask_quit {
            return;
        }
        let names = self.ytd_windows.unsaved();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::NULL.with("quit_unsaved")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.label(egui::RichText::new("Unsaved changes").strong().size(16.0));
            ui.add_space(4.0);
            ui.label(format!("Not saved: {}. Quit anyway?", names.join(", ")));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save and quit").clicked() {
                    choice = Some(0);
                }
                if ui.button(egui::RichText::new("Quit without saving").color(crate::theme::err())).clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(2);
        }
        let quit = match choice {
            // a cancelled Save as... or a failed save keeps the app open (the window says why)
            Some(0) => self.ytd_windows.save_all(),
            Some(1) => {
                self.ytd_windows.discard_all();
                true
            }
            Some(_) => false,
            None => return,
        };
        self.ask_quit = false;
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn frame_end(&mut self, ctx: &egui::Context) {
        self.check_write_back(ctx);
        for r in self.text_windows.show(ctx) {
            match r {
                text_window::Request::Handling(p) => {
                    self.tools.open_handling(&p);
                    self.mode = Mode::Tools;
                }
            }
        }
        self.ytyp_windows.show(ctx, self.game.as_ref());
        self.ytd_windows.show(ctx, &mut self.previews);
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            // during a drag from Explorer no mouse moves reach the window: keep asking where the mouse is
            ctx.request_repaint();
            if let Some((name, rect)) = self.ytd_windows.drop_target(ctx, self.mouse_pos(ctx)) {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::NULL.with("ytd_drop")));
                painter.rect_filled(rect, 8.0, egui::Color32::from_rgba_unmultiplied(20, 40, 70, 210));
                painter.rect_stroke(rect, 8.0, egui::Stroke::new(2.0, crate::theme::accent()), egui::StrokeKind::Inside);
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, format!("Drop to add to {name}"), egui::FontId::proportional(24.0), egui::Color32::WHITE);
                painter.text(rect.center() + egui::vec2(0.0, 30.0), egui::Align2::CENTER_CENTER, ".dds goes in as it is; .png .jpg ... are encoded", egui::FontId::proportional(14.0), crate::theme::text());
            }
        }
        self.previews.show(ctx);
        self.quit_question(ctx);
        if self.needs_reload {
            ctx.request_repaint();
        }
        if let Some(path) = self.ui_shot.clone() {
            // wait for a search, a texture lookup or a reveal that is still running
            if !self.explorer.busy() && self.texture_job.is_none() && self.load_job.is_none() && self.vehicle_job.is_none() && !self.explorer.editing() && !self.tools.busy() {
                self.frames += 1;
            }
            if self.frames == 6 {
                let c = ctx.clone();
                ctx.request_screenshot(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Some(out) = image::RgbaImage::from_raw(img.width() as u32, img.height() as u32, rgba) {
                        let _ = out.save(&path);
                    }
                    c.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
            ctx.request_repaint();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for (kind, p) in std::mem::take(&mut self.shot_windows) {
            let r = if kind == 0 {
                self.ytd_windows.open(&ctx, &p, false)
            } else if kind == 2 {
                self.text_windows.open(&p, false)
            } else if kind == 3 {
                self.ytyp_windows.open(&p, false)
            } else {
                std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| self.previews.open(&ctx, &p.file_name().unwrap_or_default().to_string_lossy(), &b, b.len() as u64))
            };
            if let Err(e) = r {
                self.status = e;
            }
        }
        if self.hwnd.is_none() {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            self.hwnd = frame.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                _ => None,
            });
        }
        if self.gpu.is_none() {
            if let Some(rs) = frame.wgpu_render_state() {
                self.gpu = Some(Gpu::new(rs));
                self.render = Some(rs.clone());
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        // pictures dropped on a texture dictionary window are added to it; the rest goes on as usual
        let dropped = if dropped.is_empty() { dropped } else { self.ytd_windows.take_drop(&ctx, self.mouse_pos(&ctx), dropped) };
        // with the Images to YTD page open, images and folders are for it
        let dropped = if self.mode == Mode::Tools && !dropped.is_empty() && self.tools.take_drop(&dropped) { vec![] } else { dropped };
        // with the Explorer in an archive in Edit mode, dropped files go into that folder of the archive
        let dropped = if self.mode == Mode::Explorer && !dropped.is_empty() && self.explorer.take_drop(&dropped, self.game.as_ref()) { vec![] } else { dropped };
        if !dropped.is_empty() {
            // folders go to the Explorer, files to the viewer
            let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(|p| p.is_dir());
            for d in &dirs {
                self.explorer.add_folder(d, true);
            }
            if !dirs.is_empty() && files.is_empty() {
                self.mode = Mode::Explorer;
            }
            if !files.is_empty() {
                self.mode = Mode::Viewer;
                self.accept(files);
            }
        }
        // the tree opens and closes at once
        ctx.global_style_mut(|s| s.animation_time = 0.0);
        self.poll_jobs(&ctx);
        if !ctx.egui_wants_keyboard_input() && self.mode == Mode::Viewer {
            let (frame_key, grid, textures, effects, open) = ctx.input(|i| {
                let plain = !i.modifiers.command && !i.modifiers.alt;
                (plain && i.key_pressed(egui::Key::F), plain && i.key_pressed(egui::Key::G), plain && i.key_pressed(egui::Key::T), plain && i.key_pressed(egui::Key::E), i.modifiers.command && i.key_pressed(egui::Key::O))
            });
            if frame_key {
                self.frame_model();
            }
            self.show_grid ^= grid;
            self.textured ^= textures;
            self.effects ^= effects;
            if open {
                self.open_model_dialog();
            }
        }
        if self.needs_reload {
            self.needs_reload = false;
            self.start_load(&ctx);
        }
        if let Some(g) = self.gpu.as_mut() {
            if let Some(m) = self.pending_meshes.take() {
                g.set_meshes(&m);
                let size = (self.bounds.1 - self.bounds.0).max_element().max(0.01);
                g.set_grid(size, self.bounds.0.z);
            }
            if let Some(t) = self.pending_textures.take() {
                g.set_textures(&t);
            }
        }

        egui::Panel::top("toolbar").frame(theme::panel_frame()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let tab = |ui: &mut egui::Ui, mode: &mut Mode, m: Mode, label: &str, tip: &str| {
                    let on = *mode == m;
                    let text = egui::RichText::new(label).size(14.0).color(if on { theme::text() } else { theme::dim() });
                    let b = egui::Button::new(text).fill(if on { theme::accent().gamma_multiply(0.28) } else { egui::Color32::TRANSPARENT }).stroke(egui::Stroke::NONE).corner_radius(7.0).min_size(egui::vec2(0.0, 28.0));
                    if ui.add(b).on_hover_text(tip).clicked() {
                        *mode = m;
                    }
                };
                tab(ui, &mut self.mode, Mode::Home, "Home", "What do you want to do? The start screen");
                tab(ui, &mut self.mode, Mode::Explorer, "Explorer", "Browse the game's files and your folders like classic modding tools: folders on the left, the files of the selected folder on the right");
                tab(ui, &mut self.mode, Mode::Viewer, "Viewer", "Look at a model with its textures: parts, LOD, materials");
                tab(ui, &mut self.mode, Mode::Tools, "Tools", "Handling editor, add-on pack creator, validator, liveries, textures, sirens, archives");
                tab(ui, &mut self.mode, Mode::Settings, "Settings", "Themes and general preferences");
                ui.add_space(6.0);
                ui.separator();
                if self.mode == Mode::Viewer {
                    self.viewer_toolbar(ui);
                }
            });
        });
        if self.show_game {
            let mut open = true;
            let mut load: Option<String> = None;
            egui::Window::new("Vehicle from your installed game").open(&mut open).collapsible(false).show(&ctx, |ui| {
                ui.label("Spawn name, e.g. volatus, adder, police3. Reads your own GTA V install; nothing is copied except this vehicle's files into CRCK Explorer's cache.");
                let r = ui.text_edit_singleline(&mut self.game_name);
                let go = ui.button("Load").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    load = Some(self.game_name.clone());
                }
            });
            if let Some(n) = load {
                self.show_game = false;
                self.load_vehicle_from_game(&n);
            } else if !open {
                self.show_game = false;
            }
        }
        egui::Panel::bottom("status").frame(theme::panel_frame().inner_margin(egui::Margin::symmetric(12, 4))).show(ui, |ui| {
            let err = {
                let s = self.status.to_lowercase();
                s.contains("error") || s.contains("failed") || s.starts_with("game:") || s.contains("no viewer")
            };
            ui.horizontal(|ui| {
                if self.mode == Mode::Explorer {
                    ui.label(self.explorer.status_line());
                    ui.separator();
                    if !self.explorer.message.is_empty() {
                        ui.label(&self.explorer.message);
                    }
                }
                let msg = if matches!(self.mode, Mode::Explorer | Mode::Home) && self.status.starts_with("Ready") { "" } else { self.status.as_str() };
                if err {
                    ui.colored_label(crate::theme::err(), msg);
                } else {
                    ui.label(msg);
                }
            });
        });
        if self.mode == Mode::Settings {
            let game_dir = self.game.as_ref().map(|g| g.dir.clone());
            egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(28, 16))).show(ui, |ui| self.theme_page.ui(ui, &mut self.show_home, game_dir.as_deref()));
            self.frame_end(&ctx);
            return;
        }
        if self.mode == Mode::Home {
            if self.home_logo.is_none() {
                if let Ok(img) = image::load_from_memory(include_bytes!("../../../assets/icon-256.png")) {
                    let rgba = img.to_rgba8();
                    let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
                    self.home_logo = Some(ctx.load_texture("home_logo", ci, egui::TextureOptions::LINEAR));
                }
            }
            let before = self.show_home;
            let game_dir = self.game.as_ref().map(|g| g.dir.clone());
            egui::Panel::bottom("home_footer").show(ui, |ui| home::footer(ui, game_dir.as_deref(), &mut self.show_home));
            let go = egui::CentralPanel::default().show(ui, |ui| home::show(ui, game_dir.is_some(), self.home_logo.as_ref())).inner;
            if self.show_home != before {
                settings::save_show_home(&settings::path(), self.show_home);
            }
            if let Some(go) = go {
                self.go(go);
            }
            self.frame_end(&ctx);
            return;
        }
        // the mouse's side buttons and Alt+Left / Alt+Right also walk between the Explorer and the Viewer
        let (go_back, go_fwd) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Extra1) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
                i.pointer.button_pressed(egui::PointerButton::Extra2) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)),
            )
        });
        if self.mode == Mode::Viewer && go_back && !ctx.egui_wants_keyboard_input() {
            self.mode = Mode::Explorer;
            self.viewer_forward = true;
            ctx.request_repaint();
            self.frame_end(&ctx);
            return;
        }
        if self.mode == Mode::Explorer && go_fwd && self.viewer_forward && !self.explorer.can_forward() && (self.model.is_some() || !self.ytds.is_empty()) {
            self.mode = Mode::Viewer;
            self.viewer_forward = false;
            ctx.request_repaint();
            self.frame_end(&ctx);
            return;
        }
        if self.mode == Mode::Explorer {
            let game_dir = self.game.as_ref().map(|g| g.dir.clone());
            self.explorer.set_mods(self.theme_page.warn_game_files, self.theme_page.mods_dir(game_dir.as_deref()));
            let actions = self.explorer.ui(ui, self.game.as_ref());
            self.handle_actions(&ctx, actions);
            self.frame_end(&ctx);
            return;
        }
        if self.mode == Mode::Tools {
            egui::Panel::left("tools_nav").default_size(230.0).frame(theme::panel_frame()).show(ui, |ui| self.tools.nav(ui));
            egui::CentralPanel::default().show(ui, |ui| self.tools.page_ui(ui, self.game.as_ref(), self.render.as_ref()));
            self.frame_end(&ctx);
            return;
        }
        egui::Panel::left("left").default_size(300.0).frame(theme::panel_frame()).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("< Files").on_hover_text("Back to the Explorer (also the mouse's back button or Alt+Left)").clicked() {
                    self.mode = Mode::Explorer;
                    self.viewer_forward = true;
                }
                ui.strong(if self.parts.is_empty() { "Parts".to_string() } else { format!("Parts ({})", self.parts.len()) });
            });
            ui.separator();
            self.ui_parts(ui);
        });
        let mut tex_action: Option<(usize, u8)> = None;
        let (mut add_tex, mut find_tex) = (false, false);
        egui::Panel::right("textures").default_size(290.0).frame(theme::panel_frame()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("Textures ({})", self.textures.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Add...").on_hover_text("Add .ytd texture dictionaries").clicked() {
                        add_tex = true;
                    }
                });
            });
            if !self.missing.is_empty() {
                let warn = crate::theme::warn();
                ui.label(egui::RichText::new(format!("{} texture(s) the model names are not loaded", self.missing.len())).color(warn)).on_hover_text(self.missing.join("\n"));
                ui.horizontal(|ui| {
                    let ready = self.game.is_some() && self.texture_job.is_none();
                    if ui.add_enabled(ready, egui::Button::new("Find in the game")).on_hover_text("Look for them in the game's vehicle texture dictionaries (vehshare, vehicles_*_interior, ...) and add what is found").clicked() {
                        find_tex = true;
                    }
                    if self.texture_job.is_some() {
                        ui.spinner();
                    }
                });
            }
            if self.textures.is_empty() {
                ui.label("No textures loaded yet.");
                ui.weak("A .ytd with the model's name next to it loads with the model. Otherwise use Add... or drop .ytd files on the window.");
            } else {
                ui.weak("Click a texture to enlarge it. Use ... or right-click to replace or export it.");
                egui::CollapsingHeader::new("Replace options").default_open(false).show(ui, |ui| {
                    ui.checkbox(&mut self.keep_format, "Keep format").on_hover_text("Encode a replacement PNG/JPG in the texture's current format (a DDS is always used as it is)");
                    ui.checkbox(&mut self.keep_size, "Keep size").on_hover_text("Resize a replacement PNG/JPG to the texture's current size");
                });
            }
            if !self.textures.is_empty() {
                ui.add(egui::TextEdit::singleline(&mut self.tex_filter).hint_text("Search textures").desired_width(f32::INFINITY));
            }
            let tf = self.tex_filter.trim().to_lowercase();
            let (ok, dim) = (crate::theme::ok(), crate::theme::dim());
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, t) in self.textures.iter().enumerate() {
                    if !tf.is_empty() && !t.name.to_lowercase().contains(&tf) {
                        continue;
                    }
                    let selected = self.selected_texture == Some(i);
                    let menu = |ui: &mut egui::Ui, act: &mut Option<(usize, u8)>| {
                        if t.source.is_none() {
                            ui.label("embedded in the model: not editable");
                        }
                        ui.add_enabled_ui(t.source.is_some(), |ui| {
                            if ui.button("Replace with image or DDS...").clicked() {
                                *act = Some((i, 2));
                                ui.close();
                            }
                            if ui.button("Export as DDS...").clicked() {
                                *act = Some((i, 0));
                                ui.close();
                            }
                            if ui.button("Export as PNG...").clicked() {
                                *act = Some((i, 1));
                                ui.close();
                            }
                        });
                    };
                    ui.horizontal(|ui| {
                        let bg = ui.painter().add(egui::Shape::Noop);
                        let left = ui
                            .horizontal(|ui| {
                                ui.add(egui::Image::new((t.handle.id(), egui::vec2(56.0, 56.0))));
                                ui.vertical(|ui| {
                                    ui.label(egui::RichText::new(&t.name).strong());
                                    ui.label(egui::RichText::new(format!("{}x{}  {}", t.width, t.height, t.format)).small().color(dim));
                                    if !self.parts.is_empty() {
                                        let (txt, c) = if t.used > 0 { (format!("used by {} part{}", t.used, if t.used == 1 { "" } else { "s" }), ok) } else { ("not used by this model".to_string(), dim) };
                                        ui.label(egui::RichText::new(txt).small().color(c));
                                    }
                                    let src = t.source.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "embedded in the model".to_string());
                                    ui.label(egui::RichText::new(src).small().color(dim));
                                });
                            })
                            .response;
                        let r = ui.interact(left.rect, ui.scope_id().with(("texture_row", i)), egui::Sense::click());
                        if r.clicked() {
                            self.selected_texture = if selected { None } else { Some(i) };
                        }
                        r.context_menu(|ui| menu(ui, &mut tex_action));
                        let fill = if selected { 28 } else if r.hovered() { 14 } else { 0 };
                        ui.painter().set(bg, egui::Shape::rect_filled(left.rect.expand(3.0), 6.0, crate::theme::accent().gamma_multiply(fill as f32 / 100.0)));
                        ui.menu_button("...", |ui| menu(ui, &mut tex_action));
                    });
                    if selected {
                        let w = ui.available_width();
                        ui.add(egui::Image::new((t.handle.id(), egui::vec2(w, w * t.height as f32 / t.width.max(1) as f32))));
                    }
                    ui.add_space(2.0);
                }
            });
        });
        if add_tex {
            self.add_ytd_dialog();
        }
        if find_tex {
            self.find_missing_textures();
        }
        if let Some((i, a)) = tex_action {
            self.texture_action(i, a);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let size = ui.available_size();
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
            if resp.dragged() {
                self.auto_frame = false;
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                let d = resp.drag_delta();
                self.cam.yaw -= d.x * 0.008;
                self.cam.pitch = (self.cam.pitch + d.y * 0.008).clamp(-1.5, 1.5);
            }
            if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
                let d = resp.drag_delta();
                self.cam.pan(d.x, d.y);
            }
            if resp.hovered() {
                let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    self.auto_frame = false;
                    self.cam.dist = (self.cam.dist * (1.0 - scroll * 0.0015)).clamp(0.01, 5000.0);
                }
            }
            if resp.double_clicked() {
                self.frame_model();
            }
            let ppp = ctx.pixels_per_point();
            let (w, h) = ((rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
            self.aspect = rect.width() / rect.height().max(1.0);
            // not while a new model is still loading: the parts are the old model's until it is done
            let loading = self.load_job.is_some() || self.vehicle_job.is_some();
            if !loading && !self.parts.is_empty() && (self.needs_frame || (self.auto_frame && (self.aspect - self.framed_aspect).abs() > 0.02)) {
                self.frame_model();
                self.needs_frame = false;
                self.framed_aspect = self.aspect;
            }
            if let Some(g) = self.gpu.as_mut() {
                let visible: Vec<bool> = self.parts.iter().map(|p| p.visible).collect();
                let id = g.render(w, h, self.cam.view_proj(w.max(1) as f32 / h.max(1) as f32), self.cam.eye(), &visible, self.textured, self.show_grid, self.paint, self.effects);
                ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            if self.parts.is_empty() && self.textures.is_empty() {
                // nothing loaded: one line and the two ways to start (the mode switch at the top covers the rest)
                let c = rect.center();
                let (bright, dim) = (crate::theme::text(), crate::theme::dim());
                ui.painter().text(c + egui::vec2(0.0, -60.0), egui::Align2::CENTER_CENTER, "CRCK Explorer Viewer", egui::FontId::proportional(30.0), bright);
                ui.painter().text(c + egui::vec2(0.0, -20.0), egui::Align2::CENTER_CENTER, "Open or drop a model or texture here to look at it", egui::FontId::proportional(16.0), dim);
                let size = egui::vec2(180.0, 32.0);
                let centres = if self.game.is_some() { vec![c + egui::vec2(-95.0, 28.0), c + egui::vec2(95.0, 28.0)] } else { vec![c + egui::vec2(0.0, 28.0)] };
                if ui.put(egui::Rect::from_center_size(centres[0], size), egui::Button::new("Open model...")).clicked() {
                    self.open_model_dialog();
                }
                if let Some(&c2) = centres.get(1) {
                    if ui.put(egui::Rect::from_center_size(c2, size), egui::Button::new("Vehicle from game...")).clicked() {
                        self.show_game = true;
                    }
                }
            } else if self.parts.is_empty() {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Texture dictionary: see the Textures panel on the right", egui::FontId::proportional(15.0), crate::theme::dim());
            }
            ui.painter().text(
                rect.left_bottom() + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                "drag: orbit   right-drag: pan   wheel: zoom   double-click or F: frame   G grid   T textures   E effects",
                egui::FontId::proportional(12.0),
                crate::theme::dim(),
            );
        });
        // (over a texture window the drop goes to that window, which shows its own highlight)
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) && self.ytd_windows.drop_target(&ctx, self.mouse_pos(&ctx)).is_none() {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("drop_overlay")));
            let screen = ctx.content_rect();
            painter.rect_filled(screen, 0.0, egui::Color32::from_rgba_unmultiplied(20, 40, 70, 210));
            painter.text(screen.center(), egui::Align2::CENTER_CENTER, "Drop to open", egui::FontId::proportional(30.0), egui::Color32::WHITE);
            painter.text(screen.center() + egui::vec2(0.0, 36.0), egui::Align2::CENTER_CENTER, "models: .ydr .ydd .yft     textures: .ytd", egui::FontId::proportional(15.0), crate::theme::text());
        }
        self.frame_end(&ctx);
    }
}

/// `crck-explorer --screenshot out.png [--size WxH] [--yaw deg] [--pitch deg] [--lod high|medium|low|verylow] model [ytd...]`
/// renders one frame without a window (used for automated checks and bug reports).
fn screenshot_mode(args: &[String]) -> Result<()> {
    let mut out = PathBuf::from("viewer.png");
    let (mut w, mut h) = (1280u32, 800u32);
    let (mut yaw, mut pitch) = (-0.8f32, 0.35f32);
    let mut lod = LodLevel::High;
    let mut all_parts = false;
    let mut paint = [0.62f32, 0.07, 0.07];
    let mut effects = true;
    let mut files = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--screenshot" => out = PathBuf::from(it.next().ok_or_else(|| anyhow::anyhow!("--screenshot needs a path"))?),
            "--size" => {
                let v = it.next().ok_or_else(|| anyhow::anyhow!("--size needs WxH"))?;
                let (a, b) = v.split_once('x').ok_or_else(|| anyhow::anyhow!("--size needs WxH"))?;
                (w, h) = (a.parse()?, b.parse()?);
            }
            "--yaw" => yaw = it.next().ok_or_else(|| anyhow::anyhow!("--yaw needs degrees"))?.parse::<f32>()?.to_radians(),
            "--pitch" => pitch = it.next().ok_or_else(|| anyhow::anyhow!("--pitch needs degrees"))?.parse::<f32>()?.to_radians(),
            "--all-parts" => all_parts = true,
            "--no-effects" => effects = false,
            "--paint" => {
                let v: Vec<f32> = it.next().ok_or_else(|| anyhow::anyhow!("--paint needs r,g,b (0..1)"))?.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(v.len() == 3, "--paint needs r,g,b");
                paint = [v[0], v[1], v[2]];
            }
            "--lod" => lod = it.next().ok_or_else(|| anyhow::anyhow!("--lod needs a level"))?.parse()?,
            f => files.push(PathBuf::from(f)),
        }
    }
    let is_ext = |p: &PathBuf, list: &[&str]| p.extension().map(|e| list.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false);
    let model_path = files.iter().find(|p| is_ext(p, &MODEL_EXT)).ok_or_else(|| anyhow::anyhow!("no model file given"))?.clone();
    let ytds: Vec<PathBuf> = files.iter().filter(|p| is_ext(p, &["ytd"])).cloned().collect();
    let mut meshes = model::placed_meshes(&model_path, lod)?;
    let visible: Vec<bool> = meshes.iter().map(|m| all_parts || !is_blur_plane(m)).collect();
    let (mut mn, mut mx) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for (m, _) in meshes.iter().zip(&visible).filter(|(_, v)| **v) {
        let (a, b) = bounds_of(m);
        mn = mn.min(a);
        mx = mx.max(b);
    }
    anyhow::ensure!(mn.is_finite(), "no geometry at that LOD");
    let mut seen = std::collections::HashSet::new();
    let tex: Vec<_> = model::load_textures(&model_path, &ytds)?
        .iter()
        .filter(|t| seen.insert(t.name.to_lowercase()))
        .filter_map(|t| model::texture_rgba(t).ok().map(|(w, h, d)| (t.name.clone(), w, h, d)))
        .collect();
    apply_variant_fallback(&mut meshes, &tex.iter().map(|t| t.0.clone()).collect::<Vec<_>>());
    let mut g = Gpu::headless()?;
    g.set_meshes(&meshes);
    g.set_textures(&tex);
    g.set_grid((mx - mn).max_element().max(0.01), mn.z);
    let cam = Camera { target: (mn + mx) * 0.5, yaw, pitch, dist: Camera::fit_dist(mn, mx, w as f32 / h as f32) };
    let px = g.screenshot(w, h, cam.view_proj(w as f32 / h as f32), cam.eye(), &visible, true, true, paint, effects)?;
    image::RgbaImage::from_raw(w, h, px).ok_or_else(|| anyhow::anyhow!("bad buffer"))?.save(&out)?;
    println!("{} part(s), {} texture(s) -> {}", meshes.len(), tex.len(), out.display());
    Ok(())
}

/// The logo (assets) as the window and taskbar icon.
fn window_icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../../../assets/icon-256.png")) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            egui::IconData { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }
        }
        Err(_) => egui::IconData::default(),
    }
}

/// Started from a terminal, print there like a console program would (the screenshot modes report errors on stdout).
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // SAFETY: a plain Win32 call; u32::MAX is ATTACH_PARENT_PROCESS, and it just fails when there is no parent console
    unsafe {
        AttachConsole(u32::MAX);
    }
}

fn main() -> eframe::Result {
    #[cfg(all(windows, not(debug_assertions)))]
    attach_parent_console();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--screenshot") {
        if let Err(e) = screenshot_mode(&raw) {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
        return Ok(());
    }
    let ui_shot = raw.iter().position(|a| a == "--ui-screenshot").and_then(|i| raw.get(i + 1)).map(PathBuf::from);
    let force_home = raw.iter().any(|a| a == "--home");
    let theme_arg = raw.iter().position(|a| a == "--theme").and_then(|i| raw.get(i + 1)).cloned();
    let settings_tab = raw.iter().any(|a| a == "--settings");
    let goto = raw.iter().position(|a| a == "--goto").and_then(|i| raw.get(i + 1)).cloned();
    let parts_tab = raw.iter().any(|a| a == "--tab-parts");
    let open_game = raw.iter().position(|a| a == "--open-game").and_then(|i| raw.get(i + 1)).cloned();
    let game_vehicle = raw.iter().position(|a| a == "--game-vehicle").and_then(|i| raw.get(i + 1)).cloned();
    let add_folder = raw.iter().position(|a| a == "--add-folder").and_then(|i| raw.get(i + 1)).cloned();
    let search = raw.iter().position(|a| a == "--search").and_then(|i| raw.get(i + 1)).cloned();
    let search_kind = raw.iter().position(|a| a == "--search-kind").and_then(|i| raw.get(i + 1)).cloned();
    let show_first = raw.iter().any(|a| a == "--show-first");
    let tool = raw.iter().position(|a| a == "--tool").and_then(|i| raw.get(i + 1)).cloned();
    let handling_file = raw.iter().position(|a| a == "--handling").and_then(|i| raw.get(i + 1)).cloned();
    let compare = raw.iter().position(|a| a == "--compare").and_then(|i| raw.get(i + 1)).cloned();
    // automated screenshots: open a texture dictionary or a picture in its floating window
    let ytd_window = raw.iter().position(|a| a == "--ytd-window").and_then(|i| raw.get(i + 1)).cloned();
    let picture = raw.iter().position(|a| a == "--picture").and_then(|i| raw.get(i + 1)).cloned();
    let clothing_folder = raw.iter().position(|a| a == "--clothing-folder").and_then(|i| raw.get(i + 1)).cloned();
    let rename_folder = raw.iter().position(|a| a == "--rename-folder").and_then(|i| raw.get(i + 1)).cloned();
    let rename_preset = raw.iter().position(|a| a == "--rename-preset").and_then(|i| raw.get(i + 1)).and_then(|n| n.parse::<usize>().ok());
    let ytyp_window = raw.iter().position(|a| a == "--ytyp-window").and_then(|i| raw.get(i + 1)).cloned();
    let text_window = raw.iter().position(|a| a == "--text-window").and_then(|i| raw.get(i + 1)).cloned();
    let args: Vec<PathBuf> = raw.iter().map(PathBuf::from).filter(|p| Path::new(p).exists() && Some(p.to_string_lossy().to_string()) != ytd_window && Some(p.to_string_lossy().to_string()) != ytyp_window && Some(p.to_string_lossy().to_string()) != clothing_folder && Some(p.to_string_lossy().to_string()) != rename_folder && Some(p.to_string_lossy().to_string()) != text_window && Some(p.to_string_lossy().to_string()) != rename_preset.map(|n| n.to_string()) && Some(p.to_string_lossy().to_string()) != picture && Some(p) != ui_shot.as_ref() && Some(p.to_string_lossy().to_string()) != theme_arg && Some(p.to_string_lossy().to_string()) != goto && Some(p.to_string_lossy().to_string()) != open_game && Some(p.to_string_lossy().to_string()) != add_folder && Some(p.to_string_lossy().to_string()) != handling_file).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("CRCK Explorer Viewer").with_drag_and_drop(true).with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native("CRCK Explorer Viewer", options, Box::new(move |cc| {
        let mut app = App::new(args);
        if let Some(n) = &theme_arg {
            app.theme_page.use_theme(n);
        }
        theme::apply(&cc.egui_ctx, &app.theme_page.tokens());
        // folders added to the Explorer come back next time (not in automated screenshots, which must not read or write them)
        if ui_shot.is_none() {
            app.explorer.restore(&settings::path());
        }
        app.ui_shot = ui_shot;
        app.shot_windows = ytd_window.map(|p| (0, PathBuf::from(p))).into_iter().chain(picture.map(|p| (1, PathBuf::from(p)))).chain(text_window.map(|p| (2, PathBuf::from(p)))).chain(ytyp_window.map(|p| (3, PathBuf::from(p)))).collect();
        if !app.shot_windows.is_empty() {
            app.mode = Mode::Explorer;
        }
        // with nothing to look at yet: the Home screen, or the Explorer when the Home screen was switched off
        if app.model.is_none() && app.ytds.is_empty() {
            app.mode = if app.show_home || force_home { Mode::Home } else { Mode::Explorer };
        }
        if let Some(f) = &add_folder {
            // same as the Add folder... button
            app.mode = Mode::Explorer;
            app.explorer.add_folder(Path::new(f), true);
        }
        if let Some(d) = &clothing_folder {
            app.mode = Mode::Tools;
            app.tools.open_clothing(Path::new(d));
        }
        if let Some(d) = &rename_folder {
            app.mode = Mode::Tools;
            app.tools.open_rename(Path::new(d), rename_preset);
        }
        if let Some(g) = &goto {
            // automated screenshots: show a place, names separated by /
            app.mode = Mode::Explorer;
            app.explorer.goto(g.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect());
        }
        if let Some(text) = &search {
            // same as typing it in the search box and pressing Enter; --show-first goes to the first result
            let kind = match search_kind.as_deref() {
                Some("files") => rage_toolkit_core::search::Kind::Files,
                Some("folders") => rage_toolkit_core::search::Kind::Folders,
                _ => rage_toolkit_core::search::Kind::Both,
            };
            let game = app.game.clone();
            app.mode = Mode::Explorer;
            app.explorer.search_for(text, kind, show_first, game.as_ref());
        }
        if let Some(name) = &tool {
            // automated screenshots: open the window on a tool
            app.mode = Mode::Tools;
            if let Some(p) = tools::Page::from_name(name) {
                app.tools.page = p;
            }
        }
        if let Some(f) = &handling_file {
            app.mode = Mode::Tools;
            app.tools.open_handling(Path::new(f));
            if let Some(c) = &compare {
                app.tools.compare_handling(c);
            }
        }
        if parts_tab {
            app.mode = Mode::Viewer;
        }
        if settings_tab {
            app.mode = Mode::Settings;
        }
        if let Some(full) = &open_game {
            // same path as double-clicking the file in the Explorer
            match app.full_to_loc(full) {
                Ok((loc, name)) => app.open_loc(loc, name),
                Err(e) => app.status = format!("Game: {e:#}"),
            }
        }
        if let Some(v) = &game_vehicle {
            app.load_vehicle_from_game(v);
        }
        Ok(Box::new(app))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_variant_letter_stands_in_for_a_missing_texture() {
        let loaded = vec!["hand_diff_000_a_uni".to_string(), "normal".to_string()];
        assert_eq!(variant_fallback("hand_diff_000_b_uni", &loaded).map(String::as_str), Some("hand_diff_000_a_uni"));
        assert_eq!(variant_fallback("HAND_DIFF_000_B_UNI", &loaded).map(String::as_str), Some("hand_diff_000_a_uni"));
        assert_eq!(variant_fallback("hand_diff_001_b_uni", &loaded), None, "different texture number");
        assert_eq!(variant_fallback("hand_diff_000_a_uni", &loaded), None, "the exact name is not a stand-in");
        assert_eq!(variant_fallback("normal", &loaded), None);
        assert_eq!(variant_fallback("tex_1", &["tex_2".to_string()]), None, "digits are not variant letters");
    }
}
