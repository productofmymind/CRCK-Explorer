//! The World tab: the whole map of the game in one view, and the editor for map files. Every `.ymap` of the game is read once
//! ([`WorldData`]) and drawn where it is placed, textured, out to the distance the game streams it at. Fly with the mouse and keys, select
//! entities (click, Ctrl-click, drag a box), move / turn / scale them with the gizmo, snap them to the ground or a grid, and keep them in
//! maps of a project: new maps, maps imported from files, and copies of the game's own maps. The pieces:
//!
//! - `project`: the project's maps as open documents, with a working copy each in the project folder
//! - `edit`: selection, undo over game entities and documents, add / duplicate / delete / copy / paste, the project on disk, export
//! - `gizmo`: the handles and the maths of a drag
//! - `snap`: ground snapping from the game's collision
//! - `panels`: the project explorer, the entity list with its LOD tree, the properties panels, the toolbar
mod bench;
mod edit;
mod gizmo;
mod lodinfo;
mod occlusion;
mod overlays;
mod panels;
mod project;
mod script;
mod snap;

use eframe::egui::{self, Color32, Pos2, Sense};
use edit::ClipItem;
use gizmo::{Drag, SnapMode, Space};
use glam::{Mat4, Quat, Vec3, Vec4};
use panels::{HierRows, RightTab};
use project::ProjectMaps;
use rage_toolkit_core::{
    game::Game,
    index::{FileIndex, Filter},
    job::Job,
    worldextra::{self, CollMesh},
    worldload::{self, WorldData, WorldEnt},
    worlddlc::{self, DlcLevel},
    worldmodel,
    worldproject::{self, EntityEdit, EntityKey, MapKind, Project},
    ymap_calc::{FlagLine, MapExtents},
    ymap_doc::{ExtentsMode, MapIssue, NameTable, YmapDocument},
};
use crate::{world_assets::{Loader, Shared}, world_gpu::{GpuModel, WorldGpu}};
use std::{
    collections::{HashMap, HashSet},
    sync::{mpsc, Arc},
};

const NEAR: f32 = 0.5;
const FAR: f32 = 40_000.0;
const MAX_RESULTS: usize = 300;
/// most instances drawn in one frame
const MAX_INSTANCES: usize = 150_000;

/// A camera that flies: a position and a direction, Z up.
pub struct FlyCamera {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// metres per second
    pub speed: f32,
}

impl FlyCamera {
    pub fn new() -> Self {
        Self { pos: Vec3::new(0.0, -50.0, 40.0), yaw: std::f32::consts::FRAC_PI_2, pitch: -0.4, speed: 40.0 }
    }

    pub fn forward(&self) -> Vec3 {
        Vec3::new(self.pitch.cos() * self.yaw.cos(), self.pitch.cos() * self.yaw.sin(), self.pitch.sin())
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(60f32.to_radians(), aspect.max(0.1), NEAR, FAR) * Mat4::look_to_rh(self.pos, self.forward(), Vec3::Z)
    }

    /// Look from `dist` away at `target`, from the south, a little above.
    pub fn look_at(&mut self, target: Vec3, dist: f32) {
        self.pitch = -0.5;
        self.yaw = std::f32::consts::FRAC_PI_2;
        self.pos = target - self.forward() * dist;
    }

    /// `dir`: x = right, y = forward, z = up (each -1, 0 or 1)
    pub fn fly(&mut self, dir: Vec3, dt: f32, fast: bool) {
        let fwd = self.forward();
        let right = fwd.cross(Vec3::Z).normalize_or_zero();
        let step = self.speed * if fast { 4.0 } else { 1.0 } * dt;
        self.pos += (right * dir.x + fwd * dir.y + Vec3::Z * dir.z) * step;
    }

    pub fn look(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.004;
        self.pitch = (self.pitch - dy * 0.004).clamp(-1.55, 1.55);
    }
}

/// A point in clip space to a screen position inside `rect`; `None` behind the camera.
pub fn project(vp: &Mat4, p: Vec3, rect: egui::Rect) -> Option<Pos2> {
    let c = *vp * p.extend(1.0);
    to_screen(c, rect)
}

fn to_screen(c: Vec4, rect: egui::Rect) -> Option<Pos2> {
    if c.w < NEAR * 0.5 {
        return None;
    }
    let (x, y) = (c.x / c.w, c.y / c.w);
    Some(Pos2::new(rect.left() + (x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - y * 0.5) * rect.height()))
}

/// A line between two world points, cut where it passes behind the camera; `None` when none of it is in front.
pub fn project_segment(vp: &Mat4, a: Vec3, b: Vec3, rect: egui::Rect) -> Option<(Pos2, Pos2)> {
    let (mut ca, mut cb) = (*vp * a.extend(1.0), *vp * b.extend(1.0));
    let eps = NEAR * 0.5;
    if ca.w < eps && cb.w < eps {
        return None;
    }
    if ca.w < eps {
        ca = ca + (cb - ca) * ((eps - ca.w) / (cb.w - ca.w));
    } else if cb.w < eps {
        cb = cb + (ca - cb) * ((eps - cb.w) / (ca.w - cb.w));
    }
    Some((to_screen(ca, rect)?, to_screen(cb, rect)?))
}

/// Where along the ray (origin `o`, unit direction `d`) it enters the box (centre `c`, rotation `rot`, half sizes `h`); `None` if it misses.
fn ray_box(o: Vec3, d: Vec3, c: Vec3, rot: Quat, h: Vec3) -> Option<f32> {
    let inv = rot.inverse();
    let (lo, ld) = (inv * (o - c), inv * d);
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for k in 0..3 {
        if ld[k].abs() < 1e-8 {
            if lo[k].abs() > h[k] {
                return None;
            }
            continue;
        }
        let (a, b) = ((-h[k] - lo[k]) / ld[k], (h[k] - lo[k]) / ld[k]);
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// distance from `p` to the segment `a`-`b` (screen points)
fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t - p).length()
}

/// the world-space direction of the mouse ray
fn ray_dir(vp: &Mat4, rect: egui::Rect, eye: Vec3, cursor: Pos2) -> Vec3 {
    let ndc = Vec3::new((cursor.x - rect.left()) / rect.width() * 2.0 - 1.0, 1.0 - (cursor.y - rect.top()) / rect.height() * 2.0, 1.0);
    let far = vp.inverse() * ndc.extend(1.0);
    (far.truncate() / far.w - eye).normalize_or_zero()
}

/// Where along the line `from + axis * t` the mouse ray comes closest (the t of that point); `None` when the ray runs along the line.
fn axis_param(vp: &Mat4, rect: egui::Rect, eye: Vec3, cursor: Pos2, from: Vec3, axis: Vec3) -> Option<f32> {
    let r = ray_dir(vp, rect, eye, cursor);
    let w0 = from - eye;
    let (b, d, e) = (axis.dot(r), axis.dot(w0), r.dot(w0));
    let denom = 1.0 - b * b;
    (denom.abs() > 1e-4).then(|| (b * e - d) / denom)
}

/// The 12 edges of a box with half sizes `h`, turned by `rot`, centred on `c`.
pub fn box_edges(c: Vec3, rot: Quat, h: Vec3) -> [(Vec3, Vec3); 12] {
    let corner = |i: usize| c + rot * Vec3::new(if i & 1 == 0 { -h.x } else { h.x }, if i & 2 == 0 { -h.y } else { h.y }, if i & 4 == 0 { -h.z } else { h.z });
    let mut out = [(Vec3::ZERO, Vec3::ZERO); 12];
    let mut n = 0;
    for i in 0..8usize {
        for bit in [1usize, 2, 4] {
            if i & bit == 0 {
                out[n] = (corner(i), corner(i | bit));
                n += 1;
            }
        }
    }
    out
}

/// weather presets: name, (cloud cover, fog factor, sunlight strength). An approximation, not the game's timecycle.
const WEATHERS: [(&str, [f32; 3]); 6] = [("Extra sunny", [0.05, 0.7, 1.0]), ("Clear", [0.25, 1.0, 1.0]), ("Clouds", [0.6, 1.2, 0.8]), ("Overcast", [0.95, 1.7, 0.35]), ("Smog", [0.4, 2.8, 0.65]), ("Foggy", [0.8, 6.0, 0.3])];

/// how cloudy a weather is (0 clear, 1 solid). The timecycle holds the clouds' colours but not a cover this viewer's one layer of cloud can use.
fn cloud_cover(name: &str) -> f32 {
    match name.to_ascii_uppercase().as_str() {
        "EXTRASUNNY" => 0.05,
        "CLEAR" => 0.25,
        "CLOUDS" => 0.6,
        "OVERCAST" | "THUNDER" => 0.95,
        "SMOG" => 0.4,
        "FOGGY" | "SNOWLIGHT" => 0.8,
        "RAIN" | "SNOW" => 0.9,
        "CLEARING" => 0.5,
        "NEUTRAL" => 0.3,
        "BLIZZARD" => 1.0,
        "XMAS" => 0.7,
        _ => 0.4,
    }
}

fn bookmarks_path() -> std::path::PathBuf {
    crate::settings::path().with_file_name("world_bookmarks.txt")
}

fn load_bookmarks() -> Vec<(String, [f32; 5])> {
    let text = std::fs::read_to_string(bookmarks_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let name = p.next()?.to_string();
            let v: Vec<f32> = p.filter_map(|t| t.parse().ok()).collect();
            (v.len() == 5).then(|| (name, [v[0], v[1], v[2], v[3], v[4]]))
        })
        .collect()
}

fn save_bookmarks(b: &[(String, [f32; 5])]) {
    let text: String = b.iter().map(|(n, v)| format!("{}\t{}\t{}\t{}\t{}\t{}\n", n.replace('\t', " "), v[0], v[1], v[2], v[3], v[4])).collect();
    let _ = std::fs::write(bookmarks_path(), text);
}

/// what the models are drawn as
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Textures,
    Collision,
    /// the textured models with the collision drawn over them, see-through
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// fly around; hovering and clicking do nothing
    Camera,
    /// hover to see what is under the mouse, click to select and edit
    Select,
}

/// something in the view the mouse can pick: its box in the world
#[derive(Clone, Copy)]
struct Pick {
    id: u32,
    c3: Vec3,
    rot: Quat,
    half: Vec3,
}

/// The nearest pick the ray (origin `eye`, unit direction `dir`) goes through.
fn pick_ray(picks: &[Pick], eye: Vec3, dir: Vec3) -> Option<u32> {
    let mut best = f32::MAX;
    let mut hit = None;
    for p in picks {
        // a cheap test with the box's bounding sphere first
        let radius = p.half.length();
        let oc = p.c3 - eye;
        let along = oc.dot(dir);
        if along < -radius || oc.length_squared() - along * along > radius * radius {
            continue;
        }
        if let Some(t) = ray_box(eye, dir, p.c3, p.rot, p.half) {
            if t < best {
                best = t;
                hit = Some(p.id);
            }
        }
    }
    hit
}

/// The picks whose middle is inside the screen rectangle `sel` (the view fills `rect`).
fn picks_in_rect(picks: &[Pick], vp: &Mat4, rect: egui::Rect, sel: egui::Rect) -> Vec<u32> {
    picks.iter().filter(|p| project(vp, p.c3, rect).map_or(false, |s| sel.contains(s))).map(|p| p.id).collect()
}

/// what the picture was made from: when none of it changes, the picture and the pick list are reused
#[derive(PartialEq)]
struct SceneKey {
    pos: [u32; 3],
    yaw: u32,
    pitch: u32,
    size: (u32, u32),
    view: u32,
    layers: [bool; 4],
    look: u8,
    hour: u32,
    max_lod: u8,
    wait_kids: bool,
    only_level: bool,
    occlusion: bool,
    lod_mult: u32,
    coll_wire: bool,
    coll_range: u32,
    coll_layers: [bool; 3],
    coll_interiors: bool,
    weather: u8,
    show_water: bool,
    stream_filter: bool,
    water_anim: u64,
    linear_light: bool,
    hdr_light: bool,
    fog: bool,
    show_watermap: bool,
    show_skeletons: bool,
    show_scenario: bool,
    show_scenario_chains: bool,
    show_trains: bool,
    show_heightmap: bool,
    lod_sel: u32,
    show_paths: bool,
    show_nav: bool,
    debug_view: u32,
    shadows: bool,
    rev: u64,
    /// how many models are loaded, and whether there is a loader at all (the first picture is made before it exists)
    loaded: usize,
    has_loader: bool,
}

struct Scene {
    key: SceneKey,
    tex: Option<egui::TextureId>,
    picks: Vec<Pick>,
    /// indices into `picks` of project entities whose model could not be found (drawn as boxes)
    missing: Vec<usize>,
    shown: usize,
    /// how long making it took (ms), and of that the part spent choosing what to draw and the part spent on the GPU
    ms: f32,
    gather_ms: f32,
    gpu_ms: f32,
    /// entities looked at
    visited: usize,
    levels: [u32; 7],
    waiting: u32,
    absent: u32,
    /// entities left out because an occluder hides them, and the pixels the occluders cover
    hidden: u32,
    occluded_px: usize,
}

/// one step of the undo history
#[derive(Clone)]
enum Step {
    /// placements of the game's entities: (id, before, after); `None` is "as the game has it"
    Edits(Vec<(u32, Option<EntityEdit>, Option<EntityEdit>)>),
    /// game entities taken out of the map
    DelGame(Vec<u32>),
    /// one step of the history of the document of the project map in this slot
    Doc(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Move,
    Rotate,
    Scale,
}

/// What the background thread brings: the game's file list, the readable names and the whole map.
struct Loaded {
    ix: Arc<FileIndex>,
    /// archetype hash -> name, for every hash the map or the archetype files use
    names: HashMap<u32, String>,
    data: Arc<WorldData>,
    /// lowercase file name -> game path, for models and texture dictionaries (the last copy wins)
    files: HashMap<String, String>,
    /// texture dictionary parents (`gtxd.meta`)
    parents: HashMap<String, String>,
    /// names of the archetypes some `.ytyp` defines (what can be placed), lowercase-sorted
    arch_names: Vec<String>,
    /// which dictionary of the whole game holds each texture name
    tex_index: Arc<HashMap<String, String>>,
    /// asset (or texture dictionary) -> the HD dictionary the manifests bind to it
    hd_bind: Arc<HashMap<String, String>>,
    /// the static collision files and where they lie
    bounds: Arc<Vec<(u32, [f32; 3], [f32; 3])>>,
    /// each map's streaming extents (min, max), from the game's cache files: the game loads a map while the camera is inside
    stream_boxes: Arc<Vec<([f32; 3], [f32; 3])>>,
    /// the streaming extents of each map file of the world data (by `WorldEnt::map`), when the cache lists it
    map_stream: Arc<Vec<Option<([f32; 3], [f32; 3])>>>,
    /// the map that holds the LOD parents of each map's entities (by `WorldEnt::map`): the game loads it together with the map
    map_parent: Arc<Vec<Option<u32>>>,
    /// train tracks, population zones and the heightmap
    overlays: Arc<overlays::OverlayData>,
    /// the placed interiors (from the game's cache files): name hash, position, rotation (x, y, z, w), and each interior's collision files
    interiors: Arc<Vec<(u32, [f32; 3], [f32; 4])>>,
    interior_bounds: Arc<HashMap<u32, Vec<u32>>>,
    /// collision layer (0 to 2) of each collision file by the hash of its name, from the game's cache files
    bound_layers: Arc<HashMap<u32, u32>>,
    /// the game's water quads (`water.xml`)
    water: Arc<rage_formats::WaterData>,
    /// collision files by the hash of their name -> game path
    ybn_by_hash: HashMap<u32, String>,
    /// the game's DLC packs, earliest first
    dlcs: Vec<DlcLevel>,
    /// which map files each DLC level has (archives a pack replaces leave one version)
    dlc_maps: Arc<worlddlc::DlcMaps>,
}

enum LoadState {
    NotStarted,
    Loading { rx: mpsc::Receiver<Result<Loaded, String>>, index_job: Job, world_job: Job, tex_job: Job, bounds_job: Job },
    Ready(Loaded),
    Failed(String),
}

/// Which kinds of entity are drawn.
struct Layers {
    hd: bool,
    lod: bool,
    slod: bool,
    interiors: bool,
}

/// The entities of the project's maps as the world sees them, kept between frames: one list per map, in file order (`None` for an entity
/// whose record is damaged). Made again when the maps change; a move patches only the entities it moved.
#[derive(Default)]
struct ProjCache {
    valid: bool,
    rev: u64,
    maps: Vec<Vec<Option<WorldEnt>>>,
}

pub struct World {
    state: LoadState,
    game: Option<Game>,
    query: String,
    results: Vec<u32>,
    /// the selected entities in the order they were picked; `selected` is the one picked last
    sel: Vec<u32>,
    sel_set: HashSet<u32>,
    selected: Option<u32>,
    /// where the selected entity's model file is (looked up when the selection changes)
    model: Option<(u32, Option<String>)>,
    cam: FlyCamera,
    layers: Layers,
    view_dist: f32,
    goto_text: String,
    /// saved camera places: name, x, y, z, yaw, pitch (kept in `world_bookmarks.txt` next to the settings)
    bookmarks: Vec<(String, [f32; 5])>,
    bookmark_name: String,
    gpu: Option<WorldGpu>,
    loader: Option<Loader>,
    /// frames since the map was ready (the screenshot mode waits for the first models to stream in)
    ready_frames: u32,
    // ---- the game's own entities: changes kept as an overlay
    /// the changed placements of the game's entities (index into `WorldData::ents`)
    edits: HashMap<u32, EntityEdit>,
    /// game entities taken out of the map
    deleted: HashSet<u32>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    // ---- the project's maps
    proj: ProjectMaps,
    /// bumped whenever any map changed (anything cached from the maps is stale)
    proj_rev: u64,
    /// bumped when a map changed other than by moving entities (the entity lists are stale)
    struct_rev: u64,
    proj_cache: ProjCache,
    /// the game's maps that have an editable copy in the project (their own entities are not drawn)
    hidden_maps: HashSet<u32>,
    /// the DLC level shown: the pack up to which the game's maps are drawn (None: all of them, as the game runs with every update)
    dlc_level: Option<usize>,
    /// name of the map file older versions put added entities in
    new_map: String,
    clipboard: Vec<ClipItem>,
    // ---- tools
    tool: Tool,
    mode: ViewMode,
    space: Space,
    snap_mode: SnapMode,
    /// grid step (m)
    snap: f32,
    /// angle step (degrees)
    snap_angle: f32,
    /// ground snapping also turns the entity to lie along the surface
    auto_align: bool,
    drag: Option<Drag>,
    /// the mouse is over a handle of the gizmo (a click there is not a click on the scene)
    over_gizmo: bool,
    /// where a box selection started (screen)
    rect_sel: Option<Pos2>,
    /// the mouse ray in the view this frame (origin, direction)
    cursor_ray: Option<(Vec3, Vec3)>,
    /// choosing the LOD parent of these entities: the next entity clicked becomes it
    pick_parent: Option<Vec<u32>>,
    look: Look,
    scene: Option<Scene>,
    /// bumped whenever a change to the entities means the picture has to be made again
    scene_rev: u64,
    show_extents: bool,
    /// Max LOD: 5 shows every level (orphan HD = no limit), 0 HD, 1 LOD, 2 SLOD1, 3 SLOD2, 4 SLOD3, 6 SLOD4
    max_lod: u8,
    /// result of "Check LOD links": the first problems and the total
    lod_report: Option<(Vec<String>, usize)>,
    /// "Wait for children to load": a LOD stand-in stays until the models of its children are ready
    wait_kids: bool,
    /// with a Max LOD chosen, draw only that level (not the more distant ones above it)
    only_level: bool,
    /// leave out what the game's occluders hide
    occlusion: bool,
    /// Detail distance: every LOD distance is multiplied by this
    lod_mult: f32,
    /// collision drawn as lines, and how far from the camera it is drawn
    coll_wire: bool,
    coll_range: f32,
    /// which collision layers are drawn
    coll_layers: [bool; 3],
    /// the collision of the placed interiors (MLOs)
    coll_interiors: bool,
    /// index into the timecycle's weathers (or `WEATHERS` when it could not be read)
    weather: u8,
    show_water: bool,
    /// only look at the maps the game would stream in at the camera
    stream_filter: bool,
    /// move the water's waves (the picture is made again about 15 times a second while it is on)
    animate_water: bool,
    water_t0: std::time::Instant,
    /// shade in linear light (textures to linear, lighting, tone mapping back)
    /// HDR lighting (HDR light, tone mapping) when the timecycle is read, and the distance fog (off by default)
    hdr_light: bool,
    fog: bool,
    linear_light: bool,
    /// draw the selected entity's LOD links and distances
    show_lod_links: bool,
    /// debugging overlays: the box of every drawn entity, and the spatial grid the world is cut into
    show_bounds: bool,
    show_grid: bool,
    /// train tracks, population zones, the world heightmap
    show_trains: bool,
    show_popzones: bool,
    show_heightmap: bool,
    /// the maps the game would have loaded at the camera (streaming extents around it)
    show_streaming: bool,
    show_scenario: bool,
    show_scenario_chains: bool,
    show_audio: bool,
    /// the bones of the drawn models near the camera
    show_skeletons: bool,
    /// water calming quads (where waves are damped) and wave quads (amplitude and direction)
    show_wave_quads: bool,
    show_watermap: bool,
    watermap_model: Option<Arc<GpuModel>>,
    scenario_model: Option<Arc<GpuModel>>,
    scenario_chain_model: Option<Arc<GpuModel>>,
    train_model: Option<Arc<GpuModel>>,
    height_model: Option<Arc<GpuModel>>,
    /// the water quads of the game as one blended model, made when first needed
    water_model: Option<Arc<GpuModel>>,
    /// world overlays: path nodes and links, navigation meshes
    show_paths: bool,
    show_nav: bool,
    /// shader debug view: 0 normal, 1 texture only, 2 normals, 3 tangents, 4 vertex AO, 5 texture coordinates
    debug_view: u32,
    /// time of day (hours, 0-24) for the sun and the sky
    hour: f32,
    /// the sun casts shadows near the camera (experimental)
    shadows: bool,
    // ---- panels
    right_tab: RightTab,
    hier: Option<HierRows>,
    entity_filter: String,
    /// the list and item being edited in the Map tab's "Other data" (index into `MAP_LISTS`, item)
    item_sel: (usize, Option<usize>),
    /// the extension being edited on the selected entity, and the kind the next one will be
    ext_sel: Option<usize>,
    ext_kind: usize,
    /// radius (m) of "Delete grass"
    grass_radius: f32,
    /// the light group (0 LOD lights, 1 distant lights) and light being edited in the Map tab
    light_sel: (usize, Option<usize>),
    field_bufs: HashMap<String, String>,
    map_bufs: HashMap<String, String>,
    extents_cache: Option<(usize, u64, Option<MapExtents>)>,
    flags_cache: Option<(usize, u64, Vec<FlagLine>)>,
    /// the map checked, what was found, and the state of the maps (`proj_rev`) it was found in
    validation: (Option<usize>, Vec<MapIssue>, u64),
    open_model_request: Option<String>,
    /// how often the selected entity's model is placed (counted once per model, not every frame)
    placements: std::cell::Cell<Option<(u32, usize)>>,
    add_query: String,
    /// the toolbar asked for the Add section to open and take the keyboard
    open_add: bool,
    focus_add: bool,
    /// names typed for added entities, to show them
    extra_names: HashMap<u32, String>,
    // ---- collision for snapping
    coll_cpu: HashMap<u32, Arc<CollMesh>>,
    /// the collision surface last inspected (key C) and its outline
    coll_hit: Option<snap::CollHit>,
    coll_hit_model: Option<Arc<GpuModel>>,
    ybn_paths: HashMap<u32, String>,
    // ---- the project on disk
    last_edit: std::time::Instant,
    /// when the first change since the last save was made, and the latest
    dirty_since: Option<std::time::Instant>,
    last_change: std::time::Instant,
    /// extents are made again from scratch on export (otherwise they only grow)
    recalc_extents: bool,
    /// work out flags and extents when saving
    auto_update: bool,
    /// what the last action said
    project_msg: String,
    toast: (String, std::time::Instant),
    project_loaded: bool,
    // ---- automated screenshots
    automation: bool,
    pending_move: Option<[f32; 3]>,
    pending_add: Option<String>,
    pending_tool: Option<Tool>,
    pending_demo: Option<Vec<String>>,
    pending_tab: Option<RightTab>,
    pending_bench: Option<(usize, String)>,
    bench: Option<bench::Bench>,
    /// how long the last `ui` call took (ms): the World's own share of a frame
    ui_ms: f32,
    /// the view-projection matrix and the view's rectangle of the last frame (where things are on the screen)
    last_view: Option<(Mat4, egui::Rect)>,
    script: Option<script::Script>,
    synth: Option<script::Synthetic>,
    vars: script::Vars,
    pub status: String,
}

fn lod_name(level: u8) -> &'static str {
    match level {
        0 => "HD",
        1 => "LOD",
        2 => "SLOD 1",
        3 => "SLOD 2",
        4 => "SLOD 3",
        5 => "orphan HD",
        6 => "SLOD 4",
        _ => "?",
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The box an entity occupies in the world (centre, rotation, half sizes), from its archetype's bounds (if known) and its placement.
fn entity_box_of(bounds: Option<([f32; 3], [f32; 3])>, ed: &EntityEdit) -> (Vec3, Quat, Vec3) {
    let (centre, half) = match bounds {
        Some((lo, hi)) => {
            let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
            let s = Vec3::new(ed.scale_xy, ed.scale_xy, ed.scale_z);
            (((lo + hi) * 0.5) * s, ((hi - lo) * 0.5) * s)
        }
        None => (Vec3::ZERO, Vec3::ONE),
    };
    // map entities store the inverse rotation (see `ymap::YmapEntity::to_world`)
    let rot = gizmo::world_rot(ed.rot);
    (Vec3::from(ed.pos) + rot * centre, rot, half)
}

/// Ctrl+X in Select mode: cut = copy, then delete
fn editing_cut(cmd: bool, pressed: &dyn Fn(egui::Key) -> bool, mode: ViewMode) -> bool {
    cmd && mode == ViewMode::Select && pressed(egui::Key::X)
}

/// the camera and view settings one scene is made with
struct View<'a> {
    eye: Vec3,
    fwd: Vec3,
    focal: f32,
    vp: &'a Mat4,
    rect: egui::Rect,
    view_dist: f32,
    /// hd, lod, slod, interiors
    layers: [bool; 4],
    look: Look,
    /// Max LOD: 5 (orphan HD) shows every level; another level is the most detailed one drawn
    max_lod: u8,
    wait_kids: bool,
    only_level: bool,
    lod_mult: f32,
    /// the occluders drawn for this view: an entity wholly behind them is left out
    occ: Option<&'a occlusion::Occlusion>,
    /// the game's own entities, to follow LOD parents
    ents: &'a [WorldEnt],
    /// per map file: does the game stream it in at the camera (empty: every map counts)
    active: &'a [bool],
}

/// what a scene collects while the entities are visited
#[derive(Default)]
struct Acc {
    picks: Vec<Pick>,
    missing: Vec<usize>,
    by_model: Vec<(u32, Vec<[f32; 16]>)>,
    slot: HashMap<u32, usize>,
    instances: usize,
    /// models to ask for, nearest first: (distance, archetype)
    wants: Vec<(f32, u32)>,
    /// entities looked at (those the cell test did not already leave out)
    visited: usize,
    /// instances drawn per LOD level (index = lod level), LOD stand-ins kept while their children load, entities whose model does not exist
    levels: [u32; 7],
    waiting: u32,
    absent: u32,
    hidden: u32,
    /// per entity: whether it gives way to its LOD children in this picture (0 not worked out yet, 1 yes, 2 no)
    expand: Vec<u8>,
}

/// The planes of the view frustum, the inside positive, each (unit normal, distance): from the view-projection matrix, whose depth runs 0 to 1
/// as `Mat4::perspective_rh` makes it. The far plane is not used: the view distance does that.
fn frustum_planes(vp: &Mat4) -> [Vec4; 5] {
    let (r0, r1, r2, r3) = (vp.row(0), vp.row(1), vp.row(2), vp.row(3));
    let mut planes = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2];
    for p in planes.iter_mut() {
        let len = p.truncate().length();
        if len > 0.0 {
            *p /= len;
        }
    }
    planes
}

/// Whether the box `lo..hi`, grown by `pad` metres on every side, lies wholly outside one of the planes (so nothing of it can be seen).
fn box_outside(planes: &[Vec4; 5], lo: Vec3, hi: Vec3, pad: f32) -> bool {
    planes.iter().any(|p| {
        // the corner of the box that is furthest inside this plane
        let far = Vec3::new(if p.x >= 0.0 { hi.x } else { lo.x }, if p.y >= 0.0 { hi.y } else { lo.y }, if p.z >= 0.0 { hi.z } else { lo.z });
        p.truncate().dot(far) + p.w < -pad
    })
}

/// Whether an entity with LOD children gives way to them: all its children are there (`numChildren` of them in the maps
/// streamed in here), Max LOD does not stop at its level, and the camera is within its child distance (a negative one means half its LOD distance) or
/// within the own LOD distance of any child. Worked out once per entity and picture (`memo`: 0 not yet, 1 yes, 2 no). `idx` indexes `v.ents`.
fn expands_at(v: &View, data: &WorldData, memo: &mut Vec<u8>, idx: usize) -> bool {
    let Some(x) = v.ents.get(idx) else { return false };
    if memo.len() != v.ents.len() {
        memo.clear();
        memo.resize(v.ents.len(), 0);
    }
    match memo[idx] {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let r = expands_calc(v, data, idx, x, Vec3::from(x.pos));
    memo[idx] = if r { 1 } else { 2 };
    r
}

/// `at` is where the entity stands (an edited one is not where it was stored).
fn expands_calc(v: &View, data: &WorldData, idx: usize, x: &WorldEnt, at: Vec3) -> bool {
    if x.num_children == 0 || !(v.max_lod == 5 || (x.lod_level != 5 && x.lod_level > v.max_lod)) {
        return false;
    }
    let kids = data.children_of(idx);
    // a child in a map that is not streamed in here is not there
    let there = |k: &WorldEnt| v.active.is_empty() || v.active.get(k.map as usize).copied().unwrap_or(true);
    if kids.iter().filter(|&&c| there(&v.ents[c as usize])).count() < x.num_children as usize {
        return false;
    }
    let lod_of = |k: &WorldEnt| if k.lod_dist > 0.0 { k.lod_dist } else { data.arch_of(&k.hash).map_or(0.0, |a| a.lod_dist) };
    let child_dist = if x.child_lod_dist < 0.0 { lod_of(x) * 0.5 } else { x.child_lod_dist };
    if (at - v.eye).length() <= child_dist * v.lod_mult {
        return true;
    }
    kids.iter().any(|&c| {
        let k = &v.ents[c as usize];
        there(k) && (Vec3::from(k.pos) - v.eye).length() <= lod_of(k) * v.lod_mult
    })
}

/// Whether the LOD chain above `e` keeps it from being drawn: some ancestor does not give way to its children, or the
/// top of the chain is further away than its own LOD distance. An entity's own LOD distance does not count here: once its parent gives way, it is drawn.
fn chain_hides(v: &View, data: &WorldData, memo: &mut Vec<u8>, e: &WorldEnt) -> bool {
    let mut cur = e;
    for _ in 0..8 {
        let pi = cur.parent as usize;
        let Some(pe) = v.ents.get(pi) else { return false };
        if !expands_at(v, data, memo, pi) {
            return true;
        }
        if pe.parent == worldload::NO_PARENT {
            let reach = if pe.lod_dist > 0.0 { pe.lod_dist } else { data.arch_of(&pe.hash).map_or(0.0, |a| a.lod_dist) };
            return (Vec3::from(pe.pos) - v.eye).length() > reach * v.lod_mult;
        }
        cur = pe;
    }
    false
}

/// Decide whether entity `e`, placed as `ed`, is drawn and picked in this view, and record it. `own` marks the project's entities: one whose
/// model cannot be found is still picked and shown as a box (a game entity is just left out). False once the instance limit is reached.
fn visit(v: &View, data: &WorldData, loader: &mut Loader, acc: &mut Acc, id: u32, e: &WorldEnt, ed: &EntityEdit, own: bool) -> bool {
    if !own && !v.active.is_empty() && !v.active.get(e.map as usize).copied().unwrap_or(true) {
        return true;
    }
    acc.visited += 1;
    {
        // debugging aid: CRCK_WORLD_HIDEHASH=8ae272d8,8038562e leaves those archetypes out
        static HIDE: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
        let hide = HIDE.get_or_init(|| std::env::var("CRCK_WORLD_HIDEHASH").map(|v| v.split(',').filter_map(|h| u32::from_str_radix(h.trim(), 16).ok()).collect()).unwrap_or_default());
        if !hide.is_empty() && hide.contains(&e.hash) {
            return true;
        }
    }
    // Max LOD: levels more detailed than the chosen one, and orphan HD, are not drawn
    if v.max_lod != 5 && !e.is_mlo && (e.lod_level == 5 || e.lod_level < v.max_lod) {
        return true;
    }
    if v.only_level && v.max_lod != 5 && !e.is_mlo && e.lod_level != v.max_lod {
        return true;
    }
    let on = if e.is_mlo {
        v.layers[3]
    } else {
        match e.lod_level {
            0 | 5 => v.layers[0],
            1 => v.layers[1],
            _ => v.layers[2],
        }
    };
    if !on {
        return true;
    }
    let p = Vec3::from(ed.pos);
    let dist2 = (p - v.eye).length_squared();
    if dist2 > v.view_dist * v.view_dist {
        return true;
    }
    // The LOD rule: an entity with no LOD parent is drawn out to its own LOD distance; an entity of a LOD chain is drawn whenever every
    // ancestor above it gives way to its children, whatever its own LOD distance (a child of an expanded parent is never cut by its own distance: that
    // left holes wherever the parent's child distance is longer than the child's). The root of the chain must itself be within its own distance.
    let idx = if own { usize::MAX } else { id as usize };
    let chain = !own && !(v.only_level && v.max_lod != 5) && e.parent != worldload::NO_PARENT;
    if chain {
        if chain_hides(v, data, &mut acc.expand, e) {
            return true;
        }
    } else if e.lod_dist > 0.0 && dist2 > (e.lod_dist * v.lod_mult).powi(2) {
        // the cheap case: an entity with its own LOD distance that is further away needs no archetype lookup
        return true;
    }
    // an entity is shown from the camera out to its own LOD distance, as the game streams it (`WorldData::lod_dist_of`, with one lookup)
    let arch = data.arch_of(&e.hash);
    // proxies are for reflections and shadows, not to be seen: the archetype's shadow-proxy flag and
    // the entity flag values it lists as reflection proxies
    if arch.map_or(false, |a| a.flags & 2048 != 0) || matches!(e.flags, 135790592 | 135790593 | 672661504 | 536870912 | 35127296 | 39321602) {
        return true;
    }
    let dist = dist2.sqrt();
    if !chain {
        let lod = (if e.lod_dist > 0.0 { e.lod_dist } else { arch.map_or(0.0, |a| a.lod_dist) }) * v.lod_mult;
        if dist > lod {
            return true;
        }
    }
    // a stand-in with children gives way to them once the camera is closer than their distance (unless Max LOD stops at its level)
    let gives_way = !own && if ed.pos == e.pos { expands_at(v, data, &mut acc.expand, idx) } else { expands_calc(v, data, idx, e, p) };
    if gives_way {        // wait for children: while a child's model is still on its way the stand-in stays drawn, so the
        // map has no hole; the children that are ready are drawn with it
        let mut ready = true;
        if v.wait_kids && !own {
            for &c in data.children_of(id as usize) {
                let k = &v.ents[c as usize];
                if !v.active.is_empty() && !v.active.get(k.map as usize).copied().unwrap_or(true) {
                    continue;
                }
                let skip = data.arch_of(&k.hash).map_or(true, |a| a.flags & 2048 != 0) || (v.max_lod != 5 && !k.is_mlo && (k.lod_level == 5 || k.lod_level < v.max_lod));
                if skip || loader.known(k.hash, false) {
                    continue;
                }
                // every child is drawn once the parent gives way, whatever its own LOD distance, so every child is waited for and asked
                // for now (nearest first), even when it is outside the view: the parent must not wait for a model nobody has asked for
                let kd = (Vec3::from(k.pos) - v.eye).length();
                if kd > v.view_dist {
                    continue;
                }
                ready = false;
                acc.wants.push((kd, k.hash));
            }
        }
        if ready {
            return true;
        }
        acc.waiting += 1;
    }
    let (c3, rot, half) = entity_box_of(arch.map(|a| (a.bb_min, a.bb_max)), ed);
    let radius = half.length();
    let depth = (c3 - v.eye).dot(v.fwd);
    if depth < -radius {
        return true;
    }
    let radius_px = radius * v.focal / depth.max(0.1);
    if radius_px < 1.5 {
        return true;
    }
    if let Some(c) = project(v.vp, c3, v.rect) {
        if !v.rect.expand(60.0 + radius_px.min(2000.0)).contains(c) {
            return true;
        }
    }
    // props have no collision file of their own in the game (their collision is inside the model, which this view does not read yet); the
    // world's static collision is drawn separately
    let want_model = v.look != Look::Collision;
    let model_ready = want_model && loader.get(e.hash).is_some();
    if want_model && !model_ready && !loader.known(e.hash, false) {
        // asked for after the whole view has been looked at, nearest first, so what is close loads before what is far
        acc.wants.push((dist, e.hash));
    }
    let pick = Pick { id, c3, rot, half: half.max(Vec3::splat(0.25)) };
    if want_model && !model_ready && loader.known(e.hash, false) {
        acc.absent += 1;
    }
    if !model_ready {
        // collision-only: the entities can still be picked by their boxes. A project entity whose model does not exist is shown as a box.
        let gone = own && want_model && loader.known(e.hash, false);
        if v.look == Look::Collision || gone {
            if gone {
                acc.missing.push(acc.picks.len());
            }
            acc.picks.push(pick);
            acc.instances += 1;
        }
        return true;
    }
    // a model whose pictures were cut when memory was short is loaded again at full size once it is big on the screen
    if !own && radius_px > 100.0 && loader.is_reduced(e.hash) {
        loader.upgrade(e.hash);
    }
    if let (Some(o), false) = (v.occ, own) {
        if o.hides(c3, rot, half) {
            acc.hidden += 1;
            return true;
        }
    }
    if acc.instances >= MAX_INSTANCES {
        return false;
    }
    let mut m = Mat4::from_scale_rotation_translation(Vec3::new(ed.scale_xy, ed.scale_xy, ed.scale_z), rot, p).to_cols_array();
    m[3] = e.lod_level as f32 + 1.0;
    let k = *acc.slot.entry(e.hash).or_insert_with(|| {
        acc.by_model.push((e.hash, vec![]));
        acc.by_model.len() - 1
    });
    acc.by_model[k].1.push(m);
    acc.levels[(e.lod_level as usize).min(6)] += 1;
    acc.instances += 1;
    acc.picks.push(pick);
    true
}

impl World {
    pub fn new() -> Self {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::new(200.0, -1300.0, 450.0);
        cam.pitch = -0.35;
        cam.speed = 120.0;
        let now = std::time::Instant::now();
        Self {
            state: LoadState::NotStarted,
            game: None,
            query: String::new(),
            results: vec![],
            sel: vec![],
            sel_set: HashSet::new(),
            selected: None,
            model: None,
            cam,
            // the default (max LOD: orphan HD = no limit): every level is shown, each out to its own LOD distance, and a stand-in
            // gives way to its children as the camera comes close
            layers: Layers { hd: true, lod: true, slod: true, interiors: false },
            view_dist: 4000.0,
            goto_text: String::new(),
            bookmarks: load_bookmarks(),
            bookmark_name: String::new(),
            gpu: None,
            loader: None,
            ready_frames: 0,
            edits: HashMap::new(),
            deleted: HashSet::new(),
            undo: vec![],
            redo: vec![],
            proj: ProjectMaps::new(Self::default_project_dir()),
            proj_rev: 1,
            struct_rev: 1,
            proj_cache: ProjCache::default(),
            hidden_maps: HashSet::new(),
            dlc_level: None,
            new_map: "crck_map".into(),
            clipboard: vec![],
            tool: Tool::Move,
            // you edit here: clicking selects, the right mouse button and WASD fly (Tab switches to the camera-only mode)
            mode: ViewMode::Select,
            space: Space::World,
            snap_mode: SnapMode::None,
            snap: 1.0,
            snap_angle: 5.0,
            auto_align: false,
            drag: None,
            over_gizmo: false,
            rect_sel: None,
            cursor_ray: None,
            pick_parent: None,
            look: Look::Textures,
            scene: None,
            scene_rev: 0,
            show_extents: false,
            max_lod: std::env::var("CRCK_WORLD_MAXLOD").ok().and_then(|v| v.parse().ok()).unwrap_or(5),
            wait_kids: true,
            lod_report: None,
            only_level: false,
            occlusion: std::env::var_os("CRCK_WORLD_NOOCC").is_none(),
            lod_mult: 1.0,
            coll_wire: false,
            coll_layers: [true; 3],
            coll_interiors: true,
            show_water: true,
            stream_filter: std::env::var_os("CRCK_WORLD_NOSTREAMFILTER").is_none(),
            animate_water: std::env::var_os("CRCK_WORLD_ANIMATE").is_some(),
            water_t0: std::time::Instant::now(),
            linear_light: std::env::var_os("CRCK_WORLD_NOLINEAR").is_none(),
            hdr_light: std::env::var_os("CRCK_WORLD_NOHDR").is_none(),
            fog: std::env::var_os("CRCK_WORLD_FOG").is_some(),
            show_watermap: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("watermap")),
            watermap_model: None,
            show_skeletons: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("skel")),
            show_wave_quads: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("wave")),
            show_audio: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("audio")),
            show_scenario: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("scen")),
            show_scenario_chains: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("chain")),
            scenario_model: None,
            scenario_chain_model: None,
            show_streaming: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("stream")),
            show_trains: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("trains")),
            show_popzones: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("pop")),
            show_heightmap: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("height")),
            train_model: None,
            height_model: None,
            show_bounds: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("bounds")),
            show_grid: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("grid")),
            show_lod_links: std::env::var_os("CRCK_WORLD_LODLINKS").is_some(),
            water_model: None,
            weather: std::env::var("CRCK_WORLD_WEATHER").ok().and_then(|v| v.parse().ok()).unwrap_or(1),
            show_paths: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("paths")),
            show_nav: std::env::var("CRCK_WORLD_OVERLAY").map_or(false, |v| v.contains("nav")),
            coll_range: 500.0,
            // CRCK_WORLD_SHADING / CRCK_WORLD_MAXLOD set these for automated screenshots
            debug_view: std::env::var("CRCK_WORLD_SHADING").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            hour: 12.0,
            shadows: std::env::var("CRCK_SHADOWS").is_ok(),
            right_tab: RightTab::Entity,
            hier: None,
            entity_filter: String::new(),
            item_sel: (0, None),
            ext_sel: None,
            ext_kind: 0,
            grass_radius: 10.0,
            light_sel: (1, None),
            field_bufs: HashMap::new(),
            map_bufs: HashMap::new(),
            extents_cache: None,
            flags_cache: None,
            validation: (None, vec![], 0),
            open_model_request: None,
            placements: std::cell::Cell::new(None),
            add_query: String::new(),
            open_add: false,
            focus_add: false,
            extra_names: HashMap::new(),
            coll_cpu: HashMap::new(),
            coll_hit: None,
            coll_hit_model: None,
            ybn_paths: HashMap::new(),
            last_edit: now,
            dirty_since: None,
            last_change: now,
            recalc_extents: false,
            auto_update: true,
            project_msg: String::new(),
            toast: (String::new(), now),
            project_loaded: false,
            automation: false,
            pending_move: None,
            pending_add: None,
            pending_tool: None,
            pending_demo: None,
            pending_tab: None,
            pending_bench: None,
            bench: None,
            ui_ms: 0.0,
            last_view: None,
            script: None,
            synth: None,
            vars: Default::default(),
            status: String::new(),
        }
    }

    /// still loading (the screenshot mode waits for it)
    pub fn busy(&self) -> bool {
        matches!(self.state, LoadState::Loading { .. } | LoadState::NotStarted) || self.ready_frames < 300 || (self.ready_frames < 9000 && self.loader.as_ref().map_or(false, |l| l.pending() > 0))
    }

    // ---- automation for screenshots (the `--world-*` command line flags) ----------------------------------------------------------

    pub fn queue_add(&mut self, name: String, tool: Option<String>) {
        self.automation = true;
        self.pending_add = Some(name);
        self.pending_tool = tool.map(|t| match t.as_str() {
            "rotate" => Tool::Rotate,
            "scale" => Tool::Scale,
            _ => Tool::Move,
        });
    }

    pub fn queue_look(&mut self, look: &str) {
        self.look = match look {
            "collision" => Look::Collision,
            "both" => Look::Both,
            _ => Look::Textures,
        };
    }

    pub fn queue_move(&mut self, d: [f32; 3]) {
        self.pending_move = Some(d);
    }

    /// Make a map of these models in front of the camera (the first is a LOD of the second), select two and show a panel.
    pub fn queue_demo(&mut self, names: Vec<String>, tab: Option<String>) {
        self.automation = true;
        self.pending_demo = Some(names);
        self.pending_tab = Some(if tab.as_deref() == Some("map") { RightTab::Map } else { RightTab::Entity });
    }

    /// Where to look and what to select once the map is loaded (automated screenshots).
    pub fn queue_view(&mut self, x: f32, y: f32, z: f32, select_name: Option<String>) {
        self.automation = true;
        self.cam.pos = Vec3::new(x, y, z);
        if let Some(p) = std::env::var("CRCK_WORLD_PITCH").ok().and_then(|v| v.parse::<f32>().ok()) {
            self.cam.pitch = p;
        }
        if let Some(y) = std::env::var("CRCK_WORLD_YAW").ok().and_then(|v| v.parse::<f32>().ok()) {
            self.cam.yaw = y;
        }
        self.query = select_name.unwrap_or_default();
        self.status = "queued".into();
    }

    fn start_load(&mut self, game: &Game) {
        let (tx, rx) = mpsc::channel();
        let (index_job, world_job, tex_job, bounds_job) = (Job::new(), Job::new(), Job::new(), Job::new());
        let (g, ij, wj, tj, bj) = (game.clone(), index_job.clone(), world_job.clone(), tex_job.clone(), bounds_job.clone());
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<Loaded> {
                let started = std::time::Instant::now();
                let log = std::env::var_os("CRCK_WORLD_FRAMELOG").is_some();
                let lap = |what: &str| {
                    if log {
                        eprintln!("load {:>6.0} ms: {what}", started.elapsed().as_secs_f32() * 1000.0);
                    }
                };
                let ix = FileIndex::load_or_build(&g, &ij, false)?;
                lap("file index");
                // model file names are the archetype names a map refers to: they make the hashes readable
                let mut table = NameTable::core();
                let mut files: HashMap<String, String> = HashMap::new();
                for r in ix.find("*", &Filter { exts: vec!["ydr".into(), "yft".into(), "ydd".into(), "ytd".into(), "ybn".into(), "ynd".into(), "ynv".into()], ..Default::default() }, false, usize::MAX) {
                    table.add_lowercase(r.name().rsplit_once('.').map_or(r.name(), |(s, _)| s));
                    files.insert(r.name().to_lowercase(), r.path.clone());
                }
                lap("model file names");
                // the readers that do not depend on each other run side by side with the map files, the longest part of the load
                let (parents, data, cache, manifests, overlay_data, water) = std::thread::scope(|sc| {
                    let parents = sc.spawn(|| worldmodel::txd_parents(&g, &ix));
                    // the layer of each collision file, the placed interiors and the streaming boxes, from the game's cache files (the last copy of a name wins)
                    let cache = sc.spawn(|| {
                        let mut bound_layers: HashMap<u32, u32> = HashMap::new();
                        let mut interiors: Vec<(u32, [f32; 3], [f32; 4])> = vec![];
                        let mut stream_nodes: HashMap<u32, ([f32; 3], [f32; 3])> = HashMap::new();
                        for r in ix.find("*cache_y.dat", &Filter::default(), false, usize::MAX) {
                            if let Ok(b) = g.read(&r.path) {
                                if let Ok(c) = rage_formats::parse_cache_dat(&b) {
                                    for it in c.bounds {
                                        bound_layers.insert(it.name, it.layer);
                                    }
                                    for n in c.map_nodes {
                                        stream_nodes.insert(n.name, ([n.streaming_min.x, n.streaming_min.y, n.streaming_min.z], [n.streaming_max.x, n.streaming_max.y, n.streaming_max.z]));
                                    }
                                    for p in c.interior_proxies {
                                        interiors.push((p.name, [p.position.x, p.position.y, p.position.z], [p.orientation.x, p.orientation.y, p.orientation.z, p.orientation.w]));
                                    }
                                }
                            }
                        }
                        (bound_layers, interiors, stream_nodes)
                    });
                    // each interior's collision files, from the maps' manifests (they are in the interior's own space, not the world's), and the HD texture dictionary
                    // the manifests bind to an asset (lowercase asset -> lowercase `+hi` dictionary)
                    let manifests = sc.spawn(|| {
                        let mut interior_bounds: HashMap<u32, Vec<u32>> = HashMap::new();
                        let mut hd_bind: HashMap<String, String> = HashMap::new();
                        for r in ix.find("_manifest.ymf", &Filter::default(), false, usize::MAX) {
                            if let Ok(b) = g.read(&r.path) {
                                if let Ok((_, m)) = rage_formats::ymf::parse_ymf(&b) {
                                    for h in m.hd_txd_bindings {
                                        hd_bind.insert(h.target_asset.to_lowercase(), h.hd_txd.to_lowercase());
                                    }
                                    for i in m.interiors {
                                        interior_bounds.insert(i.name.hash, i.bounds.iter().map(|h| h.hash).collect());
                                    }
                                }
                            }
                        }
                        (interior_bounds, hd_bind)
                    });
                    let overlays = sc.spawn(|| overlays::load(&g, &ix));
                    // water.xml of the map (the last copy wins, so the update's)
                    let water = sc.spawn(|| ix.find("water.xml", &Filter::default(), false, usize::MAX).into_iter().filter(|r| r.path.contains("levels/gta5/")).last().and_then(|r| g.read(&r.path).ok()).and_then(|b| rage_formats::parse_water_xml(&String::from_utf8_lossy(&b)).ok()).unwrap_or_default());
                    let data = WorldData::load(&g, &ix, &wj);
                    (parents.join().unwrap(), data, cache.join().unwrap(), manifests.join().unwrap(), overlays.join().unwrap(), water.join().unwrap())
                });
                let data = data?;
                lap("map files and the readers beside them");
                let tex_index = worldextra::texture_index(&g, &ix, &tj);
                let bounds = worldextra::bounds_index(&g, &ix, &bj);
                let (bound_layers, interiors, stream_nodes) = cache;
                let (interior_bounds, hd_bind) = manifests;
                let interior_ybn: HashSet<u32> = interior_bounds.values().flatten().copied().collect();
                let bounds: Vec<_> = bounds.into_iter().filter(|(h, _, _)| !interior_ybn.contains(h)).collect();
                let mut names: HashMap<u32, String> = HashMap::new();
                for h in data.ents.iter().map(|e| e.hash).chain(data.arch.values().flat_map(|a| [a.asset_hash, a.ydd_hash, a.txd_hash])) {
                    names.entry(h).or_insert_with(|| table.resolve(h).into_owned());
                }
                let mut arch_names: Vec<String> = data.arch.keys().filter_map(|h| names.get(h)).filter(|n| !n.starts_with("hash_")).cloned().collect();
                arch_names.sort_by_key(|n| n.to_lowercase());
                arch_names.dedup();
                lap("names");
                let dlcs = worlddlc::dlc_levels(&g, &ix);
                let dlc_maps = worlddlc::DlcMaps::new(&data.maps, &dlcs, worlddlc::dlc_map_events(&g, &ix, &dlcs));
                let ybn_by_hash: HashMap<u32, String> = files.iter().filter(|(k, _)| k.ends_with(".ybn")).map(|(k, v)| (rage_toolkit_core::rage_joaat(k.trim_end_matches(".ybn")), v.clone())).collect();
                // each map file's streaming box, by the hash of the file's name, as the cache names it
                let map_stream: Vec<Option<([f32; 3], [f32; 3])>> = data
                    .maps
                    .iter()
                    .map(|path| {
                        let leaf = path.rsplit('/').next().unwrap_or("").to_lowercase();
                        stream_nodes.get(&rage_toolkit_core::rage_joaat(leaf.trim_end_matches(".ymap"))).copied()
                    })
                    .collect();
                let stream_boxes: Vec<([f32; 3], [f32; 3])> = stream_nodes.into_values().collect();
                // the map of an entity's LOD parent, when it is another one (a map's parent map, which the game loads along with it)
                let mut map_parent: Vec<Option<u32>> = vec![None; data.maps.len()];
                for e in data.ents.iter().filter(|e| e.parent != worldload::NO_PARENT) {
                    let pm = data.ents[e.parent as usize].map;
                    if pm != e.map {
                        map_parent[e.map as usize] = Some(pm);
                    }
                }
                lap("done");
                Ok(Loaded { ix: Arc::new(ix), names, data: Arc::new(data), files, parents, arch_names, tex_index: Arc::new(tex_index), hd_bind: Arc::new(hd_bind), bounds: Arc::new(bounds), water: Arc::new(water), overlays: Arc::new(overlay_data), stream_boxes: Arc::new(stream_boxes), map_stream: Arc::new(map_stream), map_parent: Arc::new(map_parent), bound_layers: Arc::new(bound_layers), interiors: Arc::new(interiors), interior_bounds: Arc::new(interior_bounds), ybn_by_hash, dlcs, dlc_maps: Arc::new(dlc_maps) })
            })();
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
        });
        self.state = LoadState::Loading { rx, index_job, world_job, tex_job, bounds_job };
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let LoadState::Loading { rx, .. } = &self.state else { return };
        match rx.try_recv() {
            Ok(Ok(l)) => {
                self.status = format!("{} entities from {} map files", l.data.ents.len(), l.data.maps.len());
                self.ybn_paths = l.ybn_by_hash.clone();
                if std::env::var_os("CRCK_WORLD_LODCHECK").is_some() {
                    let (l_interiors_dbg, l_ib_dbg) = (l.interiors.clone(), l.interior_bounds.clone());
                    let (lines, total) = l.data.lod_problems(25);
                    eprintln!("LOD check: {total} problems");
                    if let Some(w) = &l.overlays.watermap {
                        eprintln!("watermap river0 {:?} lake0 {:?}", w.rivers.first().and_then(|r| r.first()), w.lakes.first().and_then(|l| l.1.first()));
                    }
                    eprintln!("scenario: {} files, {} points, {} chain segments", l.overlays.scenario_files, l.overlays.scenario_points.len(), l.overlays.scenario_chains.len() / 2);
                    eprintln!("HD texture bindings: {}", l.hd_bind.len());
                    eprintln!("overlays: {} trains ({} nodes), {} popzones, heightmap {}", l.overlays.trains.len(), l.overlays.trains.iter().map(|t| t.len()).sum::<usize>(), l.overlays.popzones.len(), l.overlays.heightmap.is_some());
                    for (i, e) in l.data.ents.iter().enumerate().filter(|(i, e)| *i > 7000 && e.lod_level == 1 && e.num_children > 0 && e.parent != worldload::NO_PARENT).take(2) {
                        eprintln!("SAMPLE #{i} {} at {:?} level {} kids {}", l.names.get(&e.hash).cloned().unwrap_or_default(), e.pos, e.lod_level, e.num_children);
                    }
                    for p in l_interiors_dbg.iter().filter(|p| l_ib_dbg.get(&p.0).map_or(false, |v| !v.is_empty())).take(3) {
                        eprintln!("INTERIOR {:08x} at {:?} rot {:?} files {}", p.0, p.1, p.2, l_ib_dbg[&p.0].len());
                    }
                    if let Ok(n) = std::env::var("CRCK_WORLD_FIND") {
                        let h = rage_toolkit_core::rage_joaat(&n);
                        for e in l.data.ents.iter().filter(|e| e.hash == h).take(3) {
                            eprintln!("FOUND {n} at {:?}", e.pos);
                        }
                    }
                    for x in lines {
                        eprintln!("  {x}");
                    }
                    let mut by = [0usize; 7];
                    let (mut with_kids, mut parentless_coarse) = (0, 0);
                    for e in l.data.ents.iter() {
                        by[(e.lod_level as usize).min(6)] += 1;
                        with_kids += (e.num_children > 0) as usize;
                        parentless_coarse += (e.parent == worldload::NO_PARENT && e.lod_level != 0 && e.lod_level != 5) as usize;
                    }
                    eprintln!("levels {by:?}, with children {with_kids}, coarse without parent {parentless_coarse}");
                }
                self.state = LoadState::Ready(l);
            }
            Ok(Err(e)) => self.state = LoadState::Failed(e),
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(mpsc::TryRecvError::Disconnected) => self.state = LoadState::Failed("the loading thread stopped".into()),
        }
    }

    /// What the screenshot flags asked for, once the map and the project are loaded.
    fn run_queued(&mut self) {
        if let Some(h) = std::env::var("CRCK_WORLD_HOUR").ok().and_then(|h| h.parse().ok()).filter(|_| self.automation) {
            self.hour = h;
        }
        if let Some(dir) = std::env::var_os("CRCK_WORLD_MODELS").filter(|_| self.automation) {
            self.load_model_folder(std::path::Path::new(&dir));
        }
        if let Some(names) = self.pending_demo.take() {
            self.build_demo(&names);
        }
        if let Some((n, model)) = self.pending_bench.take() {
            self.build_bench(n, &model);
        }
        if let Some(name) = self.pending_add.take() {
            let p = self.cam.pos + self.cam.forward() * 25.0;
            self.add_entity(&name, EntityEdit { pos: p.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
            if let Some(t) = self.pending_tool.take() {
                self.tool = t;
            }
            self.mode = ViewMode::Select;
        }
        if !self.query.is_empty() {
            self.search();
            if let Some(&first) = self.results.first() {
                self.select(first, true);
                if let (Some(d), Some(e)) = (self.pending_move.take(), self.ent_of(first)) {
                    let mut p = EntityEdit::of(&e);
                    p.pos = [p.pos[0] + d[0], p.pos[1] + d[1], p.pos[2] + d[2]];
                    self.set_placements(&[(first, p)]);
                    self.end_groups();
                }
            }
        }
        if let Some(t) = self.pending_tab.take() {
            self.right_tab = t;
        }
    }

    /// A small map for the screenshot flags: the models in a row in front of the camera, the first the LOD of the second, the last two selected.
    fn build_demo(&mut self, names: &[String]) {
        let Some(slot) = self.new_map_in_project(Some("demo")) else { return };
        let origin = self.cam.pos + self.cam.forward() * 25.0;
        let mut ids = vec![];
        for (k, n) in names.iter().enumerate() {
            let p = origin + Vec3::new((k as f32 - names.len() as f32 / 2.0) * 7.0, 0.0, 0.0);
            if let Some(id) = self.add_entity(n, EntityEdit { pos: p.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 }) {
                ids.push(id);
            }
        }
        if ids.len() >= 2 {
            self.doc_op(slot, |d| d.set_lod_level(0, 1));
            self.doc_op(slot, |d| d.set_parent(1, Some(0)));
        }
        self.end_groups();
        let take = ids.len().min(2);
        self.set_selection(ids[ids.len() - take..].to_vec());
        self.mode = ViewMode::Select;
        self.layers.lod = true;
        self.show_extents = true;
        self.bump_docs();
    }

    /// Once the map is loaded and there is a GPU: start the model streaming.
    fn ensure_renderer(&mut self, game: Option<&Game>, rs: Option<&eframe::egui_wgpu::RenderState>) {
        if self.loader.is_some() {
            return;
        }
        let (LoadState::Ready(l), Some(rs), Some(game)) = (&self.state, rs, game) else { return };
        let gpu = WorldGpu::new(rs);
        let shared = Arc::new(Shared { game: game.clone(), files: l.files.clone(), names: l.names.clone(), data: l.data.clone(), parents: l.parents.clone(), tex_index: l.tex_index.clone(), hd_bind: l.hd_bind.clone(), ybn_by_hash: l.ybn_by_hash.clone(), loose: Default::default(), loose_names: Default::default() });
        self.loader = Some(Loader::new(shared, gpu.factory()));
        if let Some(i) = std::env::var("CRCK_WORLD_SELECT").ok().and_then(|v| v.parse::<u32>().ok()) {
            self.set_selection(vec![i]);
        }
        self.gpu = Some(gpu);
    }

    fn name(&self, hash: u32) -> String {
        match &self.state {
            LoadState::Ready(l) => l.names.get(&hash).cloned().or_else(|| self.extra_names.get(&hash).cloned()).unwrap_or_else(|| format!("hash_{hash:08X}")),
            _ => format!("hash_{hash:08X}"),
        }
    }

    /// the entities whose model name contains the query, nearest to the camera first
    fn search(&mut self) {
        let LoadState::Ready(l) = &self.state else { return };
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            self.results.clear();
            return;
        }
        let hit: HashSet<u32> = l.names.iter().filter(|(_, n)| n.to_lowercase().contains(&q)).map(|(h, _)| *h).collect();
        let eye = self.cam.pos;
        let mut found: Vec<(f32, u32)> = l
            .data
            .ents
            .iter()
            .enumerate()
            .filter(|(_, e)| hit.contains(&e.hash))
            .map(|(i, e)| ((Vec3::from(e.pos) - eye).length_squared(), i as u32))
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.results = found.into_iter().take(MAX_RESULTS).map(|(_, i)| i).collect();
    }

    /// Select only `id`; with `fly` the camera goes to it and the view is switched to Select mode (picking from a list means you want to work on it).
    fn select(&mut self, id: u32, fly: bool) {
        self.set_selection(vec![id]);
        if fly {
            self.mode = ViewMode::Select;
            self.focus_selection();
        }
        let LoadState::Ready(l) = &self.state else { return };
        let Some(e) = self.ent_of(id) else { return };
        // the model file: a drawable or fragment by its asset name, or the .ydd that holds it
        let a = l.data.arch_of(&e.hash);
        let (stem, exts): (String, &[&str]) = match a {
            Some(a) if a.asset_type == 3 && a.ydd_hash != 0 => (self.name(a.ydd_hash), &["ydd"]),
            Some(a) if a.asset_type == 1 => (self.name(if a.asset_hash != 0 { a.asset_hash } else { e.hash }), &["yft"]),
            Some(a) => (self.name(if a.asset_hash != 0 { a.asset_hash } else { e.hash }), &["ydr", "yft"]),
            None => (self.name(e.hash), &["ydr", "yft"]),
        };
        let mut path = None;
        for ext in exts {
            let file = format!("{stem}.{ext}");
            let filter = Filter { exts: vec![(*ext).into()], ..Default::default() };
            if let Some(r) = l.ix.find(&file, &filter, false, 50).into_iter().filter(|r| r.name().eq_ignore_ascii_case(&file)).last() {
                path = Some(r.path.clone());
                break;
            }
        }
        self.model = Some((id, path));
    }

    /// Fly the camera to the selection: far enough back to see all of it.
    fn focus_selection(&mut self) {
        let Some(data) = self.data() else { return };
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for &id in &self.sel {
            let (Some(e), Some(p)) = (self.ent_of(id), self.placement_of(id)) else { continue };
            let (c, _, h) = entity_box_of(data.arch_of(&e.hash).map(|a| (a.bb_min, a.bb_max)), &p);
            lo = lo.min(c - h.max(Vec3::splat(1.0)));
            hi = hi.max(c + h.max(Vec3::splat(1.0)));
        }
        if lo.x > hi.x {
            return;
        }
        let r = (hi - lo).length() * 0.5;
        self.cam.look_at((lo + hi) * 0.5, (r * 3.0).clamp(20.0, 600.0));
    }

    // ---- the frame ---------------------------------------------------------------------------------------------------------------

    /// Returns the game path of a model the user asked to open in the Viewer.
    pub fn ui(&mut self, ui: &mut egui::Ui, game: Option<&Game>, rs: Option<&eframe::egui_wgpu::RenderState>) -> Option<String> {
        let ctx = ui.ctx().clone();
        let started = std::time::Instant::now();
        if self.game.is_none() {
            self.game = game.cloned();
        }
        if matches!(self.state, LoadState::NotStarted) {
            if let Some(g) = game {
                self.start_load(g);
            }
        }
        self.poll(&ctx);
        self.ensure_renderer(game, rs);
        if matches!(self.state, LoadState::Ready(_)) {
            self.ready_frames = self.ready_frames.saturating_add(1);
        }
        if let Some(l) = self.loader.as_mut() {
            if l.poll() {
                ctx.request_repaint();
            }
            if l.pending() > 0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        if matches!(self.state, LoadState::Ready(_)) && !self.project_loaded {
            let dir = if self.automation {
                // a screenshot run works in a scratch project, never the user's
                let d = std::env::temp_dir().join(format!("crck_world_auto_{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&d);
                d
            } else {
                Self::last_project_dir()
            };
            self.load_project_from(dir);
            self.run_queued();
        }
        self.keys(&ctx);
        self.autosave(&ctx);
        egui::Panel::left("world_left").default_size(310.0).frame(crate::theme::panel_frame()).show(ui, |ui| self.left_panel(ui));
        egui::Panel::right("world_right").default_size(320.0).frame(crate::theme::panel_frame()).show(ui, |ui| self.right_panel(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(crate::theme::bg())).show(ui, |ui| self.viewport(ui));
        self.bench_tick(&ctx);
        self.script_tick(&ctx);
        self.ui_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.open_model_request.take()
    }

    /// The keyboard: undo, save, copy, paste, duplicate, select all, delete, the tools, the mode.
    fn keys(&mut self, ctx: &egui::Context) {
        // only typing in a text box keeps the shortcuts away (a focused button must not: focus lands on one after a click or a Tab)
        if ctx.text_edit_focused() {
            return;
        }
        let (cmd, shift, alt) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
        let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        let (copy_ev, paste_ev) = ctx.input(|i| (i.events.iter().any(|e| matches!(e, egui::Event::Copy)), i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))));
        if cmd && pressed(egui::Key::Z) {
            if shift {
                self.redo_step();
            } else {
                self.undo_step();
            }
        } else if cmd && pressed(egui::Key::Y) {
            self.redo_step();
        }
        if cmd && pressed(egui::Key::S) {
            self.save_project(true);
        }
        if (cmd && pressed(egui::Key::C)) || copy_ev {
            self.copy_selection();
        }
        if editing_cut(cmd, &pressed, self.mode) {
            self.copy_selection();
            self.delete_selection();
        }
        if (cmd && pressed(egui::Key::V)) || paste_ev {
            let at = self.cursor_point().unwrap_or_else(|| self.cam.pos + self.cam.forward() * 25.0);
            self.paste(at);
        }
        if cmd && pressed(egui::Key::D) {
            self.duplicate_selection(Vec3::new(2.0, 0.0, 0.0));
        }
        if cmd && pressed(egui::Key::A) {
            let slot = self.proj.active;
            if let Some(m) = self.proj.maps.get(slot) {
                let ids: Vec<u32> = (0..m.doc.len()).map(|i| project::id(slot, i)).collect();
                self.set_selection(ids);
            }
        }
        if alt && pressed(egui::Key::W) {
            self.tool = Tool::Move;
        }
        if alt && pressed(egui::Key::E) {
            self.tool = Tool::Rotate;
        }
        if alt && pressed(egui::Key::R) {
            self.tool = Tool::Scale;
        }
        // what changes or removes entities only works in Select mode, where the handles are shown and the selection is in front of you
        let editing = self.mode == ViewMode::Select;
        if editing && pressed(egui::Key::Delete) {
            self.delete_selection();
        }
        if editing && !cmd && !alt && pressed(egui::Key::G) {
            self.place_selection_at_cursor();
        }
        if !cmd && !alt && pressed(egui::Key::F) {
            self.focus_selection();
        }
        if !cmd && !alt && pressed(egui::Key::C) {
            self.inspect_collision();
        }
        if editing && (pressed(egui::Key::PageUp) || pressed(egui::Key::PageDown)) {
            let step = if shift { 1.0 } else { 0.1 };
            let dz = if pressed(egui::Key::PageUp) { step } else { -step };
            let ids = self.sel.clone();
            self.nudge(&ids, Vec3::new(0.0, 0.0, dz));
        }
        // Tab is taken in `take_tab` before the window sees it
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            if self.pick_parent.take().is_some() || self.rect_sel.take().is_some() {
                // cancelled what was under way
            } else {
                self.mode = ViewMode::Camera;
            }
        }
    }

    /// Tab switches between Camera and Select mode. The window calls this with the keys of the frame before egui sees them: egui would give
    /// the key to the first button it finds (its keyboard navigation), so the key is taken out of the list. Returns true when it was taken.
    pub fn take_tab(&mut self, events: &mut Vec<egui::Event>) -> bool {
        let tabbed = events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. } if !modifiers.any()));
        if tabbed {
            events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Tab, modifiers, .. } if !modifiers.any()));
            self.mode = if self.mode == ViewMode::Camera { ViewMode::Select } else { ViewMode::Camera };
        }
        tabbed
    }

    /// Where the mouse points: the game's collision there, else the horizontal plane through the selection (or the camera).
    fn cursor_point(&mut self) -> Option<Vec3> {
        let (o, d) = self.cursor_ray?;
        if let Some((t, _)) = self.ray_hit(o, d, 4000.0) {
            return Some(o + d * t);
        }
        let z = self.sel.first().and_then(|&i| self.placement_of(i)).map_or(0.0, |p| p.pos[2]);
        (d.z.abs() > 1e-4).then(|| (z - o.z) / d.z).filter(|t| *t > 0.0 && *t < 4000.0).map(|t| o + d * t)
    }

    // ---- the scene ---------------------------------------------------------------------------------------------------------------

    /// Work out what is visible from the camera and draw it: the entities near the camera that pass the layer and LOD rules, grouped
    /// by model, sent to the GPU as one picture. Models that are not loaded yet are asked for. Returns the picture and what can be picked.
    /// The weathers to choose from: the game's timecycle files, or the fixed presets when they could not be read.
    fn weather_names(&self) -> Vec<String> {
        if let LoadState::Ready(l) = &self.state {
            if !l.overlays.timecycle.is_empty() {
                return l.overlays.timecycle.names().iter().map(|n| n.chars().next().map(|c| c.to_string() + &n[1..].to_ascii_lowercase()).unwrap_or_default()).collect();
            }
        }
        WEATHERS.iter().map(|w| w.0.to_string()).collect()
    }

    /// The weather's (cloud cover, fog factor, sunlight), and with the timecycle read the light, sky and fog it gives at this hour.
    fn atmosphere(&self) -> ([f32; 3], Option<rage_toolkit_core::timecycle::Sky>) {
        if let LoadState::Ready(l) = &self.state {
            let tc = &l.overlays.timecycle;
            if !tc.is_empty() {
                let i = self.weather as usize % tc.cycles.len();
                return ([cloud_cover(&tc.cycles[i].name), 1.0, 1.0], tc.sample(i, self.hour));
            }
        }
        (WEATHERS[self.weather as usize % WEATHERS.len()].1, None)
    }

    fn build_scene(&mut self, data: &Arc<WorldData>, vp: &Mat4, rect: egui::Rect, ppp: f32, key: SceneKey) -> Scene {
        let (weather, tc_sky) = self.atmosphere();
        let started = std::time::Instant::now();
        self.ensure_proj_cache();
        let eye = self.cam.pos;
        // the maps the game would have loaded here: the camera inside their streaming box; entities of the others are
        // not looked at
        let active_maps: Vec<bool> = match (&self.state, self.stream_filter) {
            (LoadState::Ready(l), true) => {
                let mut on: Vec<bool> = l.map_stream.iter().map(|b| b.map_or(true, |(lo, hi)| (0..3).all(|k| eye[k] >= lo[k] && eye[k] <= hi[k]))).collect();
                // the game loads the parent maps of those too, and theirs in turn: a LOD stand-in whose map the camera is
                // outside of still gives way to its children, and still shows where they are not drawn
                for m in 0..on.len() {
                    let mut cur = m;
                    while on[cur] {
                        match l.map_parent.get(cur).copied().flatten() {
                            Some(p) if !on[p as usize] => {
                                on[p as usize] = true;
                                cur = p as usize;
                            }
                            _ => break,
                        }
                    }
                }
                // maps that are hidden (the project's own copies, the DLC level's replaced versions) are not looked at at all
                for &m in &self.hidden_maps {
                    if let Some(a) = on.get_mut(m as usize) {
                        *a = false;
                    }
                }
                on
            }
            _ => vec![],
        };
        // the game's occluders within 500 m and in the view, drawn into a small depth picture
        let occ = match (&self.state, self.occlusion) {
            (LoadState::Ready(l), true) if l.overlays.occluders.total > 0 => {
                let planes = frustum_planes(vp);
                let visible = |lo: Vec3, hi: Vec3| !box_outside(&planes, lo, hi, 0.0);
                Some(occlusion::Occlusion::build(vp, rect.width() / rect.height().max(1.0), l.overlays.occluders.near(eye, 500.0, &visible)))
            }
            _ => None,
        };
        let view = View {
            eye,
            fwd: self.cam.forward(),
            focal: rect.height() * 0.5 / (30f32.to_radians()).tan(),
            vp,
            rect,
            view_dist: self.view_dist,
            layers: [self.layers.hd, self.layers.lod, self.layers.slod, self.layers.interiors],
            look: self.look,
            max_lod: self.max_lod,
            wait_kids: self.wait_kids,
            only_level: self.only_level,
            lod_mult: self.lod_mult,
            occ: occ.as_ref(),
            ents: &data.ents,
            active: &active_maps,
        };
        let mut acc = Acc::default();
        let mut world_coll: Vec<u32> = vec![];
        // the collision of placed interiors: the file and where it is placed
        let mut interior_coll: Vec<(u32, [f32; 16])> = vec![];
        if let Some(loader) = self.loader.as_mut() {
            loader.new_pass();
            // ---- the entities of the project's maps first: they are what is being edited, so their models are asked for before the game's own
            // (which would otherwise fill the loading queue)
            'maps: for (slot, list) in self.proj_cache.maps.iter().enumerate() {
                for (index, e) in list.iter().enumerate() {
                    let Some(e) = e else { continue };
                    let ed = EntityEdit { pos: e.pos, rot: e.rot, scale_xy: e.scale_xy, scale_z: e.scale_z };
                    if !visit(&view, data, loader, &mut acc, project::id(slot, index), e, &ed, true) {
                        break 'maps;
                    }
                }
            }
            // ---- the game's entities, from the grid cells near the camera
            let edited: Vec<u32> = self.edits.keys().copied().collect();
            let (want_hd, want_other) = (self.layers.hd, self.layers.lod || self.layers.slod || self.layers.interiors);
            // cells wholly outside the view are skipped (the map is about -400 to 1,800 m high)
            let planes = frustum_planes(vp);
            // with the game's streaming rule only the entities of the maps that stream in here are looked at, taken from each map's own list; else the
            // grid cells in reach and in the view
            let stream_list: Vec<u32> = if active_maps.is_empty() { vec![] } else { active_maps.iter().enumerate().filter(|(_, on)| **on).flat_map(|(m, _)| data.ents_of_map(m).iter().copied()).collect() };
            let mut cells: Vec<&[u32]> = if active_maps.is_empty() {
                data.near_layers_visible(eye.x, eye.y, self.view_dist, want_hd, want_other, &|lo, hi, reach| !box_outside(&planes, Vec3::new(lo[0], lo[1], -400.0), Vec3::new(hi[0], hi[1], 1800.0), reach))
            } else {
                vec![stream_list.as_slice()]
            };
            cells.push(edited.as_slice());
            let last_cell = cells.len() - 1;
            let (has_edits, has_deleted, has_hidden) = (!self.edits.is_empty(), !self.deleted.is_empty(), !self.hidden_maps.is_empty());
            'cells: for (ci, cell) in cells.into_iter().enumerate() {
                for &i in cell {
                    if has_deleted && self.deleted.contains(&i) {
                        continue;
                    }
                    // an edited entity is visited once, in the extra list at the end (it may have left its own cell)
                    if has_edits && ci != last_cell && self.edits.contains_key(&i) {
                        continue;
                    }
                    let e = &data.ents[i as usize];
                    if has_hidden && self.hidden_maps.contains(&e.map) {
                        continue;
                    }
                    let ed = if has_edits { self.edits.get(&i).copied().unwrap_or_else(|| EntityEdit::of(e)) } else { EntityEdit::of(e) };
                    if !visit(&view, data, loader, &mut acc, i, e, &ed, false) {
                        break 'cells;
                    }
                }
            }
            // ---- ask for the models the view needs, nearest first (the queue is short: what is close must not wait behind what is far)
            acc.wants.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut asked = HashSet::new();
            for (_, h) in std::mem::take(&mut acc.wants) {
                if asked.insert(h) && !loader.want(h) {
                    break;
                }
            }
            // ---- the world's static collision: the files whose box is within the view distance, each placed as it is (it is in world coordinates)
            if self.look != Look::Textures {
                if let LoadState::Ready(l) = &self.state {
                    let mut near: Vec<(f32, u32)> = vec![];
                    for (h, lo, hi) in l.bounds.iter() {
                        let d = |e: f32, lo: f32, hi: f32| (lo - e).max(0.0).max(e - hi);
                        let dist = (d(eye.x, lo[0], hi[0]).powi(2) + d(eye.y, lo[1], hi[1]).powi(2) + d(eye.z, lo[2], hi[2]).powi(2)).sqrt();
                        let layer = l.bound_layers.get(h).copied().unwrap_or(0) as usize;
                        if dist <= self.view_dist.min(self.coll_range) && self.coll_layers.get(layer).copied().unwrap_or(true) {
                            near.push((dist, *h));
                        }
                    }
                    // nearest first, so what you are looking at arrives before what is far off
                    near.sort_by(|a, b| a.0.total_cmp(&b.0));
                    for (_, h) in near {
                        if loader.get_collision(h).is_some() {
                            world_coll.push(h);
                        } else if !loader.want_collision(h) {
                            break;
                        }
                    }
                    // interiors: their collision files are in the interior's own space, placed by the interior's position and rotation
                    if self.coll_interiors {
                        let reach = self.view_dist.min(self.coll_range);
                        let mut near_int: Vec<(f32, usize)> = l.interiors.iter().enumerate().map(|(i, p)| ((Vec3::from(p.1) - eye).length(), i)).filter(|(d, _)| *d <= reach).collect();
                        near_int.sort_by(|a, b| a.0.total_cmp(&b.0));
                        'ints: for (_, i) in near_int.into_iter().take(40) {
                            let (name, pos, rot) = &l.interiors[i];
                            let Some(files) = l.interior_bounds.get(name) else { continue };
                            let m = Mat4::from_rotation_translation(Quat::from_xyzw(rot[0], rot[1], rot[2], rot[3]).normalize(), Vec3::from(*pos)).to_cols_array();
                            for &h in files {
                                if loader.get_collision(h).is_some() {
                                    interior_coll.push((h, m));
                                } else if !loader.want_collision(h) {
                                    break 'ints;
                                }
                            }
                        }
                    }
                }
            }
        }
        let gather_ms = started.elapsed().as_secs_f32() * 1000.0;
        let mut tex = None;
        let lod_segs = self.lod_overlay_segments();
        if let (Some(gpu), Some(loader)) = (self.gpu.as_mut(), self.loader.as_mut()) {
            let mut matrices: Vec<[f32; 16]> = Vec::with_capacity(acc.instances * 2);
            let (mut groups, mut coll_groups) = (vec![], vec![]);
            if self.show_water {
                if self.water_model.is_none() {
                    if let LoadState::Ready(l) = &self.state {
                        let (mut pos, mut idx) = (vec![], vec![]);
                        for q in l.water.quads.iter().filter(|q| !q.invisible) {
                            let b = pos.len() as u32;
                            pos.extend([[q.min_x, q.min_y, q.z], [q.max_x, q.min_y, q.z], [q.max_x, q.max_y, q.z], [q.min_x, q.max_y, q.z]]);
                            idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
                        }
                        let f = gpu.factory();
                        f.add_texture("water", 1, 1, &[28, 78, 104, 190]);
                        let normals = vec![[0.0, 0.0, 1.0]; pos.len()];
                        self.water_model = f.part(&pos, &normals, &[], &idx, &[], &[], Some("water"), (None, None), 4).map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                }
                if let Some(m) = &self.water_model {
                    groups.push((m.clone(), matrices.len() as u32, 1));
                    matrices.push(Mat4::IDENTITY.to_cols_array());
                }
            }
            // the bones of the models within 150 m, as lines in the world
            let mut skeleton_segs: Vec<[f32; 3]> = vec![];
            for (h, list) in std::mem::take(&mut acc.by_model) {
                if self.show_skeletons && skeleton_segs.len() < 60_000 {
                    if let Some(bones) = loader.skeleton(h) {
                        for m in list.iter().take(60) {
                            let mut m = *m;
                            // the fourth row's first slot carries the LOD level for the colour view
                            m[3] = 0.0;
                            let m = Mat4::from_cols_array(&m);
                            if (m.w_axis.truncate() - eye).length() > 150.0 {
                                continue;
                            }
                            for (a, b) in bones.iter() {
                                skeleton_segs.push(m.transform_point3(Vec3::from(*a)).to_array());
                                skeleton_segs.push(m.transform_point3(Vec3::from(*b)).to_array());
                            }
                        }
                    }
                }
                if let Some(model) = loader.get(h) {
                    groups.push((model.clone(), matrices.len() as u32, list.len() as u32));
                    matrices.extend(list);
                }
            }
            // each world collision file is one instance with no transform
            let identity = Mat4::IDENTITY.to_cols_array();
            for h in world_coll {
                if let Some(model) = loader.get_collision(h) {
                    coll_groups.push((model.clone(), matrices.len() as u32, 1));
                    matrices.push(identity);
                }
            }
            for (h, m) in interior_coll {
                if let Some(model) = loader.get_collision(h) {
                    coll_groups.push((model.clone(), matrices.len() as u32, 1));
                    matrices.push(m);
                }
            }
            // overlays of lines: path nodes (512 m cells) and navigation meshes (150 m cells) within 600 m
            let mut line_groups = vec![];
            if let Some(p) = gpu.factory().line_part(&skeleton_segs, [255, 210, 60, 255], "line:skel") {
                line_groups.push((Arc::new(GpuModel { parts: vec![p] }), matrices.len() as u32, 1));
                matrices.push(identity);
            }
            if let LoadState::Ready(l) = &self.state {
                if self.show_trains {
                    if self.train_model.is_none() {
                        self.train_model = gpu.factory().line_part(&overlays::train_segments(&l.overlays), [255, 120, 40, 255], "line:train").map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                    if let Some(m) = &self.train_model {
                        line_groups.push((m.clone(), matrices.len() as u32, 1));
                        matrices.push(identity);
                    }
                }
                if self.show_watermap {
                    if self.watermap_model.is_none() {
                        self.watermap_model = l.overlays.watermap.as_ref().and_then(|w| gpu.factory().line_part(&overlays::watermap_segments(w), [90, 200, 255, 255], "line:watermap")).map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                    if let Some(m) = &self.watermap_model {
                        line_groups.push((m.clone(), matrices.len() as u32, 1));
                        matrices.push(identity);
                    }
                }
                if self.show_scenario {
                    if self.scenario_model.is_none() {
                        self.scenario_model = gpu.factory().line_part(&overlays::scenario_segments(&l.overlays), [255, 90, 220, 255], "line:scen").map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                    if let Some(m) = &self.scenario_model {
                        line_groups.push((m.clone(), matrices.len() as u32, 1));
                        matrices.push(identity);
                    }
                }
                if self.show_scenario_chains {
                    if self.scenario_chain_model.is_none() {
                        self.scenario_chain_model = gpu.factory().line_part(&l.overlays.scenario_chains, [170, 120, 255, 255], "line:chain").map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                    if let Some(m) = &self.scenario_chain_model {
                        line_groups.push((m.clone(), matrices.len() as u32, 1));
                        matrices.push(identity);
                    }
                }
                if self.show_heightmap {
                    if self.height_model.is_none() {
                        self.height_model = l.overlays.heightmap.as_ref().and_then(|h| gpu.factory().line_part(&overlays::heightmap_segments(h), [120, 230, 120, 255], "line:height")).map(|p| Arc::new(GpuModel { parts: vec![p] }));
                    }
                    if let Some(m) = &self.height_model {
                        line_groups.push((m.clone(), matrices.len() as u32, 1));
                        matrices.push(identity);
                    }
                }
            }
            {
                for (on, kind) in [(self.show_paths, 0u8), (self.show_nav, 1u8)] {
                    if !on {
                        continue;
                    }
                    for cell in worldextra::overlay_cells(kind, eye.x - 600.0, eye.y - 600.0, eye.x + 600.0, eye.y + 600.0) {
                        if let Some(m) = loader.lines(kind, cell) {
                            line_groups.push((m, matrices.len() as u32, 1));
                            matrices.push(identity);
                        }
                    }
                }
            }
            for (k, (seg, colour)) in lod_segs.into_iter().zip([[60, 220, 255, 255], [80, 230, 90, 255], [255, 230, 50, 255], [255, 140, 30, 255]]).enumerate() {
                if let Some(p) = gpu.factory().line_part(&seg, colour, &format!("line:lod{k}")) {
                    line_groups.push((Arc::new(GpuModel { parts: vec![p] }), matrices.len() as u32, 1));
                    matrices.push(identity);
                }
            }
            if let Some(m) = &self.coll_hit_model {
                line_groups.push((m.clone(), matrices.len() as u32, 1));
                matrices.push(identity);
            }
            let alpha = if self.look == Look::Both { 0.45 } else { 1.0 };
            tex = Some(gpu.render((rect.width() * ppp) as u32, (rect.height() * ppp) as u32, *vp, eye, &matrices, &groups, &coll_groups, alpha, self.coll_wire, &line_groups, self.debug_view, weather, self.linear_light, self.water_t0.elapsed().as_secs_f32(), self.hour, self.shadows, tc_sky.as_ref(), self.hdr_light, self.fog));
        }
        if let (Ok(px), LoadState::Ready(l)) = (std::env::var("CRCK_WORLD_RAYPX"), &self.state) {
            // debugging aid: every entity of the map whose box the ray through that pixel of the view (x,y from the view's top left) goes through, and what became of it
            let xy: Vec<f32> = px.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            if xy.len() == 2 {
                eprintln!("RAYSCENE");
                let ndc = glam::Vec4::new(xy[0] / rect.width() * 2.0 - 1.0, 1.0 - xy[1] / rect.height() * 2.0, 1.0, 1.0);
                let far = vp.inverse() * ndc;
                let dir = (far.truncate() / far.w - eye).normalize();
                let drawn: std::collections::HashSet<u32> = acc.picks.iter().map(|p| p.id).collect();
                let mut hits: Vec<(f32, String)> = vec![];
                for (i, e) in data.ents.iter().enumerate() {
                    let arch = data.arch_of(&e.hash);
                    let ed = EntityEdit { pos: e.pos, rot: e.rot, scale_xy: e.scale_xy, scale_z: e.scale_z };
                    let (c3, rot, half) = entity_box_of(arch.map(|a| (a.bb_min, a.bb_max)), &ed);
                    let inv = rot.inverse();
                    let (o, d) = (inv * (eye - c3), inv * dir);
                    let (mut t0, mut t1) = (0.0f32, 4000.0f32);
                    let mut hit = true;
                    for k in 0..3 {
                        if d[k].abs() < 1e-6 {
                            if o[k].abs() > half[k] {
                                hit = false;
                            }
                        } else {
                            let (u, w) = ((-half[k] - o[k]) / d[k], (half[k] - o[k]) / d[k]);
                            t0 = t0.max(u.min(w));
                            t1 = t1.min(u.max(w));
                        }
                    }
                    if hit && t0 <= t1 && half.x < 600.0 {
                        let nm = l.names.get(&e.hash).cloned().unwrap_or_default();
                        let map = data.maps.get(e.map as usize).map_or(String::new(), |m| format!("{m:?}")).rsplit('/').next().unwrap_or("").to_string();
                        let dist = (c3 - eye).length();
                        hits.push((t0, format!("t={t0:.0}..{t1:.0} lod {} {nm} ({:08x}) guid {:x} at {:.2},{:.2},{:.2} lod_dist {} child {} kids {} dist {dist:.0} half {:.0},{:.0},{:.0} drawn {} model {} map {map}", e.lod_level, e.hash, e.guid, e.pos[0], e.pos[1], e.pos[2], e.lod_dist, e.child_lod_dist, e.num_children, half.x, half.y, half.z, drawn.contains(&(i as u32)), self.loader.as_ref().map_or(false, |ld| ld.get(e.hash).is_some()))));
                    }
                }
                hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                for (_, h) in hits.iter().take(80) {
                    eprintln!("RAYHIT {h}");
                }
            }
        }
        let ms = started.elapsed().as_secs_f32() * 1000.0;
        if std::env::var_os("CRCK_WORLD_FRAMELOG").is_some() {
            // instances of one model at exactly the same place (the same map data loaded twice)
            let (twins, pairs) = {
                let mut seen: HashMap<(u32, i32, i32, i32), u32> = HashMap::new();
                let mut pairs: HashMap<(String, String), usize> = HashMap::new();
                let name = |m: u32| data.maps.get(m as usize).map_or(String::new(), |p| p.rsplit('/').next().unwrap_or("").split('_').next().unwrap_or("").to_lowercase());
                let mut n = 0;
                for p in &acc.picks {
                    let Some(e) = data.ents.get(p.id as usize) else { continue };
                    let key = (e.hash, (p.c3.x * 20.0).round() as i32, (p.c3.y * 20.0).round() as i32, (p.c3.z * 20.0).round() as i32);
                    if let Some(&first) = seen.get(&key) {
                        n += 1;
                        *pairs.entry((name(first), name(e.map))).or_default() += 1;
                    } else {
                        seen.insert(key, e.map);
                    }
                }
                let mut pairs: Vec<_> = pairs.into_iter().collect();
                pairs.sort_by(|a, b| b.1.cmp(&a.1));
                pairs.truncate(8);
                (n, pairs)
            };
            eprintln!("twins (first map, second map): {pairs:?}");
            let mut by_prefix: HashMap<String, usize> = HashMap::new();
            for p in &acc.picks {
                if let Some(m) = data.ents.get(p.id as usize).and_then(|e| data.maps.get(e.map as usize)) {
                    *by_prefix.entry(m.rsplit('/').next().unwrap_or("").split('_').next().unwrap_or("").to_lowercase()).or_default() += 1;
                }
            }
            let mut top: Vec<_> = by_prefix.into_iter().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            eprintln!("drawn by map name prefix: {:?}", &top[..top.len().min(14)]);
            eprintln!("scene: {} drawn ({twins} of them twins) of {} looked at in {} maps, {} waiting for children, choose {gather_ms:.1} ms, GPU {:.1} ms, total {ms:.1} ms", acc.instances, acc.visited, active_maps.iter().filter(|on| **on).count(), acc.waiting, ms - gather_ms);
        }
        Scene { key, tex, picks: acc.picks, missing: acc.missing, shown: acc.instances, ms, gather_ms, gpu_ms: ms - gather_ms, visited: acc.visited, levels: acc.levels, waiting: acc.waiting, absent: acc.absent, hidden: acc.hidden, occluded_px: occ.as_ref().map_or(0, |o| o.covered()) }
    }

    /// The box of the active map's extents, stored and calculated, drawn in the view.
    fn draw_extents(&mut self, painter: &egui::Painter, vp: &Mat4, rect: egui::Rect) {
        if !self.show_extents {
            return;
        }
        let slot = self.proj.active;
        let Some(stored) = self.proj.maps.get(slot).and_then(|m| m.doc.stored_extents()) else { return };
        let calc = self.calc_extents_cached(slot);
        let green = Color32::from_rgb(80, 215, 90);
        let orange = Color32::from_rgb(255, 160, 40);
        let mut boxes: Vec<(MapExtents, bool)> = vec![(stored, true)];
        if let Some(c) = calc {
            if c != stored {
                boxes.push((c, false));
            }
        }
        for (ex, is_stored) in boxes {
            for (lo, hi, col) in [(ex.entities_min, ex.entities_max, green), (ex.streaming_min, ex.streaming_max, orange)] {
                let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
                if lo.x > hi.x || lo.y > hi.y || lo.z > hi.z {
                    continue;
                }
                for (a, b) in box_edges((lo + hi) * 0.5, Quat::IDENTITY, (hi - lo) * 0.5) {
                    if let Some((p, q)) = project_segment(vp, a, b, rect) {
                        if is_stored {
                            painter.line_segment([p, q], egui::Stroke::new(2.0, col));
                        } else {
                            painter.extend(egui::Shape::dashed_line(&[p, q], egui::Stroke::new(1.2, col.gamma_multiply(0.6)), 6.0, 5.0));
                        }
                    }
                }
            }
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let select_mode = self.mode == ViewMode::Select;
        // ---- camera input: right mouse (or, in Camera mode, the left one) looks, W A S D Q E fly, Shift is fast, the wheel changes the speed
        let looking = resp.dragged_by(egui::PointerButton::Secondary) || (!select_mode && resp.dragged_by(egui::PointerButton::Primary));
        if looking {
            let d = resp.drag_delta();
            self.cam.look(d.x, d.y);
        }
        if resp.hovered() || looking {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.speed = (self.cam.speed * 1.002f32.powf(scroll)).clamp(1.0, 5000.0);
            }
            let (dir, dt, fast) = ctx.input(|i| {
                let k = |key| i.key_down(key) as i32 as f32;
                (Vec3::new(k(egui::Key::D) - k(egui::Key::A), k(egui::Key::W) - k(egui::Key::S), k(egui::Key::E) - k(egui::Key::Q)), i.stable_dt.min(0.1), i.modifiers.shift)
            });
            if dir != Vec3::ZERO && !ctx.input(|i| i.modifiers.alt || i.modifiers.command) {
                self.cam.fly(dir, dt, fast);
                ctx.request_repaint();
            }
        }
        let aspect = rect.width() / rect.height().max(1.0);
        let vp = self.cam.view_proj(aspect);
        let painter = ui.painter_at(rect);
        let eye = self.cam.pos;
        self.last_view = Some((vp, rect));
        let data = match &self.state {
            LoadState::Ready(l) => l.data.clone(),
            _ => {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Loading the map...", egui::FontId::proportional(16.0), crate::theme::dim());
                return;
            }
        };
        self.cursor_ray = resp.hover_pos().map(|c| (eye, ray_dir(&vp, rect, eye, c)));
        // ---- the picture of the models: worked out again only when something that changes it has changed
        let key = SceneKey {
            pos: eye.to_array().map(f32::to_bits),
            yaw: self.cam.yaw.to_bits(),
            pitch: self.cam.pitch.to_bits(),
            size: (rect.width() as u32, rect.height() as u32),
            view: self.view_dist.to_bits(),
            layers: [self.layers.hd, self.layers.lod, self.layers.slod, self.layers.interiors],
            look: self.look as u8,
            hour: self.hour.to_bits(),
            max_lod: self.max_lod,
            wait_kids: self.wait_kids,
            only_level: self.only_level,
            occlusion: self.occlusion,
            lod_mult: self.lod_mult.to_bits(),
            coll_wire: self.coll_wire,
            coll_range: self.coll_range.to_bits(),
            coll_layers: self.coll_layers,
            coll_interiors: self.coll_interiors,
            weather: self.weather,
            show_water: self.show_water,
            stream_filter: self.stream_filter,
            water_anim: if self.show_water && self.animate_water { (self.water_t0.elapsed().as_secs_f32() * 15.0) as u64 + 1 } else { 0 },
            linear_light: self.linear_light,
            hdr_light: self.hdr_light,
            fog: self.fog,
            show_watermap: self.show_watermap,
            show_skeletons: self.show_skeletons,
            show_scenario: self.show_scenario,
            show_scenario_chains: self.show_scenario_chains,
            show_trains: self.show_trains,
            show_heightmap: self.show_heightmap,
            lod_sel: if self.show_lod_links { self.sel.first().map_or(0, |i| i + 1) ^ (self.lod_mult.to_bits() >> 3) } else { 0 },
            show_paths: self.show_paths,
            show_nav: self.show_nav,
            debug_view: self.debug_view,
            shadows: self.shadows,
            rev: self.scene_rev,
            loaded: self.loader.as_ref().map_or(0, |l| l.loaded() + l.arrivals() * 1_000_003),
            has_loader: self.loader.is_some(),
        };
        if self.animate_water && self.show_water {
            ctx.request_repaint_after(std::time::Duration::from_millis(66));
        }
        if self.scene.as_ref().map_or(true, |s| s.key != key) {
            let ppp = ctx.pixels_per_point();
            let s = self.build_scene(&data, &vp, rect, ppp, key);
            self.scene = Some(s);
        }
        if let Some(tex) = self.scene.as_ref().and_then(|s| s.tex) {
            let mut mesh = egui::Mesh::with_texture(tex);
            mesh.add_rect_with_uv(rect, egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            painter.add(egui::Shape::mesh(mesh));
        }
        let accent = crate::theme::accent();
        self.draw_extents(&painter, &vp, rect);
        // ---- what is under the mouse (Select mode only): the nearest entity whose box the mouse ray goes through
        let mut hovered: Option<u32> = None;
        if select_mode && self.drag.is_none() && self.rect_sel.is_none() && !self.over_gizmo {
            if let (Some(cur), Some(scene)) = (resp.hover_pos(), self.scene.as_ref()) {
                hovered = pick_ray(&scene.picks, eye, ray_dir(&vp, rect, eye, cur));
            }
        }
        // ---- boxes: project entities without a model, the selection, what is under the mouse
        if let Some(scene) = self.scene.as_ref() {
            for &k in &scene.missing {
                let p = &scene.picks[k];
                for (a, b) in box_edges(p.c3, p.rot, p.half) {
                    if let Some((s, t)) = project_segment(&vp, a, b, rect) {
                        painter.line_segment([s, t], egui::Stroke::new(1.2, Color32::from_rgb(230, 190, 80)));
                    }
                }
            }
            let mut drawn = 0;
            for p in &scene.picks {
                let is_sel = self.sel_set.contains(&p.id);
                let is_hover = hovered == Some(p.id) && !is_sel;
                if !(is_sel || is_hover) {
                    continue;
                }
                drawn += 1;
                if drawn > 2500 {
                    break;
                }
                let (col, w) = if is_sel { (accent, if Some(p.id) == self.selected { 2.5 } else { 1.8 }) } else { (Color32::from_white_alpha(200), 1.5) };
                for (a, b) in box_edges(p.c3, p.rot, p.half) {
                    if let Some((s, t)) = project_segment(&vp, a, b, rect) {
                        painter.line_segment([s, t], egui::Stroke::new(w, col));
                    }
                }
            }
        }
        // ---- debugging overlays: every entity's box, the spatial grid
        if self.show_bounds {
            if let Some(scene) = self.scene.as_ref() {
                for p in scene.picks.iter().take(1500) {
                    for (a, b) in box_edges(p.c3, p.rot, p.half) {
                        if let Some((s, t)) = project_segment(&vp, a, b, rect) {
                            painter.line_segment([s, t], egui::Stroke::new(0.8, Color32::from_rgba_unmultiplied(160, 200, 255, 110)));
                        }
                    }
                }
            }
        }
        if self.show_grid {
            let cell = worldload::CELL;
            let (cx, cy) = worldload::cell_of(eye.x, eye.y);
            let z = eye.z - 30.0;
            for k in -6..=7 {
                let x = (cx + k) as f32 * cell;
                let y0 = (cy - 6) as f32 * cell;
                let y1 = (cy + 7) as f32 * cell;
                if let Some((s, t)) = project_segment(&vp, Vec3::new(x, y0, z), Vec3::new(x, y1, z), rect) {
                    painter.line_segment([s, t], egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 200, 80, 120)));
                }
                let y = (cy + k) as f32 * cell;
                let x0 = (cx - 6) as f32 * cell;
                let x1 = (cx + 7) as f32 * cell;
                if let Some((s, t)) = project_segment(&vp, Vec3::new(x0, y, z), Vec3::new(x1, y, z), rect) {
                    painter.line_segment([s, t], egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 200, 80, 120)));
                }
            }
        }
        if self.show_streaming {
            if let LoadState::Ready(l) = &self.state {
                let mut n = 0;
                for (lo, hi) in l.stream_boxes.iter() {
                    if eye.x < lo[0] || eye.x > hi[0] || eye.y < lo[1] || eye.y > hi[1] {
                        continue;
                    }
                    n += 1;
                    if n > 400 {
                        break;
                    }
                    let (lo, hi) = (Vec3::from(*lo), Vec3::from(*hi));
                    for (a, b) in box_edges((lo + hi) * 0.5, Quat::IDENTITY, (hi - lo) * 0.5) {
                        if let Some((s, t)) = project_segment(&vp, a, b, rect) {
                            painter.line_segment([s, t], egui::Stroke::new(0.8, Color32::from_rgba_unmultiplied(255, 120, 200, 90)));
                        }
                    }
                }
            }
        }
        if self.show_wave_quads {
            if let LoadState::Ready(l) = &self.state {
                let rect_lines = |painter: &egui::Painter, min: [f32; 2], max: [f32; 2], col: Color32| {
                    let z = 0.5;
                    let p = [Vec3::new(min[0], min[1], z), Vec3::new(max[0], min[1], z), Vec3::new(max[0], max[1], z), Vec3::new(min[0], max[1], z)];
                    for k in 0..4 {
                        if let Some((s, t)) = project_segment(&vp, p[k], p[(k + 1) % 4], rect) {
                            painter.line_segment([s, t], egui::Stroke::new(1.2, col));
                        }
                    }
                };
                for q in l.water.calming_quads.iter().filter(|q| ((q.min_x + q.max_x) * 0.5 - eye.x).abs() < 3000.0 && ((q.min_y + q.max_y) * 0.5 - eye.y).abs() < 3000.0) {
                    rect_lines(&painter, [q.min_x, q.min_y], [q.max_x, q.max_y], Color32::from_rgba_unmultiplied(120, 220, 255, 160));
                }
                for q in l.water.wave_quads.iter().filter(|q| ((q.min_x + q.max_x) * 0.5 - eye.x).abs() < 3000.0 && ((q.min_y + q.max_y) * 0.5 - eye.y).abs() < 3000.0) {
                    rect_lines(&painter, [q.min_x, q.min_y], [q.max_x, q.max_y], Color32::from_rgba_unmultiplied(255, 200, 90, 160));
                    // the wave direction from the middle, as long as the amplitude says
                    let c = Vec3::new((q.min_x + q.max_x) * 0.5, (q.min_y + q.max_y) * 0.5, 0.5);
                    let d = Vec3::new(q.x_direction, q.y_direction, 0.0).normalize_or_zero() * (10.0 + q.amplitude * 20.0);
                    if let Some((s, t)) = project_segment(&vp, c, c + d, rect) {
                        painter.line_segment([s, t], egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 200, 90, 220)));
                    }
                }
            }
        }
        if self.show_audio {
            if let LoadState::Ready(l) = &self.state {
                let mut drawn = 0;
                for z in l.overlays.audio_zones.iter() {
                    let c = Vec3::from(z.act_centre);
                    let reach = Vec3::from(z.act_size).max_element() * 0.5;
                    if (c - eye).length() > 2500.0 + reach {
                        continue;
                    }
                    drawn += 1;
                    if drawn > 300 {
                        break;
                    }
                    for (centre, size, angle, col) in [(z.act_centre, z.act_size, z.act_angle, Color32::from_rgba_unmultiplied(140, 230, 90, 130)), (z.pos_centre, z.pos_size, z.pos_angle, Color32::from_rgba_unmultiplied(60, 210, 200, 110))] {
                        let (c, half) = (Vec3::from(centre), Vec3::from(size) * 0.5);
                        let mut seg: Vec<(Vec3, Vec3)> = vec![];
                        if z.shape == 1 {
                            // a sphere: the size's x is its radius; three circles
                            let r = size[0];
                            for (a, b) in [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)] {
                                let pt = |i: usize| c + a * (r * (i as f32 / 24.0 * std::f32::consts::TAU).cos()) + b * (r * (i as f32 / 24.0 * std::f32::consts::TAU).sin());
                                for i in 0..24 {
                                    seg.push((pt(i), pt(i + 1)));
                                }
                            }
                        } else {
                            seg.extend(box_edges(c, Quat::from_rotation_z(angle.to_radians()), half));
                        }
                        for (a, b) in seg {
                            if let Some((s, t)) = project_segment(&vp, a, b, rect) {
                                painter.line_segment([s, t], egui::Stroke::new(1.0, col));
                            }
                        }
                    }
                }
            }
        }
        if self.show_popzones {
            if std::env::var_os("CRCK_WORLD_DEBUG").is_some() {
                eprintln!("popzones: eye {eye:?}");
            }
            if let LoadState::Ready(l) = &self.state {
                let z = eye.z - 30.0;
                for (name, label, r) in l.overlays.popzones.iter() {
                    let c = Vec3::new((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5, z);
                    if (c - eye).truncate().length() > 1500.0 + (r[2] - r[0]).max(r[3] - r[1]) {
                        continue;
                    }
                    let col = Color32::from_rgba_unmultiplied(120, 200, 255, 150);
                    let pts = [Vec3::new(r[0], r[1], z), Vec3::new(r[2], r[1], z), Vec3::new(r[2], r[3], z), Vec3::new(r[0], r[3], z)];
                    for k in 0..4 {
                        if let Some((s, t)) = project_segment(&vp, pts[k], pts[(k + 1) % 4], rect) {
                            painter.line_segment([s, t], egui::Stroke::new(1.0, col));
                        }
                    }
                    if (c - eye).truncate().length() < 700.0 {
                        if let Some(p) = project(&vp, c, rect) {
                            painter.text(p, egui::Align2::CENTER_CENTER, format!("{name} {label}"), egui::FontId::proportional(11.0), col);
                        }
                    }
                }
            }
        }
        // ---- the gizmo on the selection, and the box selection
        let captured = if select_mode && !self.sel.is_empty() {
            self.gizmo(&ctx, &resp, &painter, &vp, rect, eye)
        } else {
            self.drag = None;
            self.over_gizmo = false;
            false
        };
        let mods = ctx.input(|i| i.modifiers);
        let add_to = mods.command || mods.shift;
        if select_mode && !captured {
            if resp.drag_started_by(egui::PointerButton::Primary) && !self.over_gizmo {
                self.rect_sel = ctx.input(|i| i.pointer.press_origin());
            }
            if let (Some(start), Some(cur)) = (self.rect_sel, ctx.pointer_interact_pos()) {
                let r = egui::Rect::from_two_pos(start, cur);
                painter.rect_filled(r, 0.0, accent.gamma_multiply(0.15));
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
                if resp.drag_stopped() {
                    if r.width() > 3.0 || r.height() > 3.0 {
                        let hits = self.scene.as_ref().map(|s| picks_in_rect(&s.picks, &vp, rect, r)).unwrap_or_default();
                        let mut list = if add_to { self.sel.clone() } else { vec![] };
                        list.extend(hits);
                        self.set_selection(list);
                    }
                    self.rect_sel = None;
                }
            } else if resp.drag_stopped() {
                self.rect_sel = None;
            }
        } else if !select_mode {
            self.rect_sel = None;
        }
        // ---- a click: select, add to the selection, or choose a LOD parent
        if select_mode && !captured && !self.over_gizmo && resp.clicked_by(egui::PointerButton::Primary) {
            match hovered {
                Some(id) => {
                    if let Some(children) = self.pick_parent.take() {
                        self.assign_parent(&children, Some(id));
                    } else if add_to {
                        self.toggle_select(id);
                    } else {
                        self.select(id, false);
                    }
                }
                None => {
                    if self.pick_parent.take().is_none() && !add_to {
                        self.set_selection(vec![]);
                    }
                }
            }
        }
        if let (Some(h), Some(cur)) = (hovered, resp.hover_pos()) {
            if let Some(e) = self.ent_of(h) {
                let map = match project::split(h) {
                    Some((s, _)) => self.proj.maps.get(s).map(|m| m.name.clone()).unwrap_or_default(),
                    None => leaf(&data.maps[e.map as usize]).to_string(),
                };
                let verb = if self.pick_parent.is_some() { "  ·  click to make it the parent" } else { "" };
                let text = format!("{}  ·  {map}{verb}", self.name(e.hash));
                let galley = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), crate::theme::text());
                let pos = cur + egui::vec2(16.0, -26.0);
                painter.rect_filled(egui::Rect::from_min_size(pos - egui::vec2(6.0, 3.0), galley.size() + egui::vec2(12.0, 6.0)), 6.0, Color32::from_black_alpha(190));
                painter.galley(pos, galley, crate::theme::text());
            }
        }
        self.toolbar(&ctx, rect);
        // ---- hints and readout, bottom left (the toolbar and the side panels keep the other corners free)
        let hint = if self.pick_parent.is_some() {
            "Click the parent entity in the view or in the list  ·  Esc: cancel".to_string()
        } else if select_mode {
            "Select  ·  click: select (Ctrl: add)  ·  drag: box  ·  Alt+W / E / R: move / rotate / scale  ·  G: put under cursor  ·  Ctrl+D copy  ·  Delete  ·  Tab: camera".to_string()
        } else {
            "Camera  ·  drag or right mouse: look  ·  W A S D: move  ·  Q E: down / up  ·  Shift: fast  ·  wheel: speed  ·  Tab: select".to_string()
        };
        let (loaded, pending) = self.loader.as_ref().map_or((0, 0), |l| (l.loaded(), l.pending()));
        let (shown, ms) = self.scene.as_ref().map_or((0, 0.0), |s| (s.shown, s.ms));
        let sel_text = if self.sel.is_empty() { String::new() } else { format!("   ·   {} selected", self.sel.len()) };
        let readout = format!("{:.0}, {:.0}, {:.0}   ·   {shown} drawn   ·   {loaded} models{}   ·   {:.0} m/s   ·   view {ms:.0} ms{sel_text}", eye.x, eye.y, eye.z, if pending > 0 { format!(" (+{pending} loading)") } else { String::new() }, self.cam.speed);
        let wrap = (rect.width() - 40.0).max(120.0);
        let g_hint = painter.layout(hint, egui::FontId::proportional(12.5), crate::theme::text(), wrap);
        let g_read = painter.layout(readout, egui::FontId::monospace(12.0), crate::theme::dim(), wrap);
        let h = g_hint.size().y + g_read.size().y + 16.0;
        let w = g_hint.size().x.max(g_read.size().x) + 16.0;
        let top_left = rect.left_bottom() + egui::vec2(12.0, -12.0 - h);
        painter.rect_filled(egui::Rect::from_min_size(top_left, egui::vec2(w, h)), 6.0, Color32::from_black_alpha(160));
        let read_pos = top_left + egui::vec2(8.0, 6.0);
        let hint_pos = read_pos + egui::vec2(0.0, g_read.size().y + 4.0);
        painter.galley(read_pos, g_read, crate::theme::dim());
        painter.galley(hint_pos, g_hint, crate::theme::text());
        // ---- what the last action said, for a few seconds
        if self.project_msg != self.toast.0 {
            self.toast = (self.project_msg.clone(), std::time::Instant::now());
        }
        if !self.toast.0.is_empty() {
            let age = self.toast.1.elapsed().as_secs_f32();
            if age < 6.0 {
                let g = painter.layout(self.toast.0.clone(), egui::FontId::proportional(13.0), crate::theme::text(), (rect.width() * 0.5).max(160.0));
                let size = g.size() + egui::vec2(20.0, 14.0);
                // stacked above the hints
                let at = egui::Rect::from_min_size(egui::pos2(top_left.x, top_left.y - 8.0 - size.y), size);
                painter.rect_filled(at, 8.0, Color32::from_black_alpha(205));
                painter.galley(at.min + egui::vec2(10.0, 7.0), g, crate::theme::text());
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))
    }

    fn lod_ent(x: f32, level: u8, lod_dist: f32, child_lod_dist: f32, kids: u32, parent: u32) -> WorldEnt {
        WorldEnt { pos: [x, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0, lod_dist, child_lod_dist, num_children: kids, hash: 1, flags: 0, guid: 0, map: 0, index: 0, lod_level: level, is_mlo: false, parent }
    }

    fn lod_view<'a>(eye_x: f32, data: &'a WorldData, vp: &'a Mat4, active: &'a [bool]) -> View<'a> {
        View { eye: Vec3::new(eye_x, 0.0, 0.0), fwd: Vec3::X, focal: 1000.0, vp, rect: rect(), view_dist: 5000.0, layers: [true; 4], look: Look::Textures, max_lod: 5, wait_kids: false, only_level: false, lod_mult: 1.0, occ: None, ents: &data.ents, active }
    }

    #[test]
    fn the_children_of_a_stand_in_that_gives_way_are_drawn_whatever_their_own_lod_distance() {
        // an SLOD root (shown to 500 m, gives way to its child from 400 m), a LOD 1 entity under it (450 m, gives way from 100 m to its two HD children,
        // which are shown to 80 m each when they stand alone)
        let data = WorldData::assemble(
            vec![],
            vec![],
            vec![lod_ent(0.0, 2, 500.0, 400.0, 1, worldload::NO_PARENT), lod_ent(0.0, 1, 450.0, 100.0, 2, 0), lod_ent(5.0, 0, 80.0, 0.0, 0, 1), lod_ent(-5.0, 0, 80.0, 0.0, 0, 1)],
            HashMap::new(),
            vec![],
        );
        let vp = Mat4::IDENTITY;
        // (does entity 0 give way, does entity 1 give way, is entity 1 left out by its chain, is entity 2 left out by its chain) at each distance
        let at = |eye: f32| {
            let v = lod_view(eye, &data, &vp, &[]);
            let mut memo = vec![];
            (expands_at(&v, &data, &mut memo, 0), expands_at(&v, &data, &mut memo, 1), chain_hides(&v, &data, &mut memo, &data.ents[1]), chain_hides(&v, &data, &mut memo, &data.ents[2]))
        };
        // 90 m: both stand-ins give way, the HD children are drawn though 90 m is beyond their own 80 m (the hole this once left)
        assert_eq!(at(90.0), (true, true, false, false));
        // 300 m: the root gives way to the LOD 1 entity, which stays (its children are too far), so the HD children are not drawn
        assert_eq!(at(300.0), (true, false, false, true));
        // 480 m: the root stays; everything below it is left out
        assert_eq!(at(480.0), (false, false, true, true));
        // 450 m: the LOD 1 entity is within its own distance, which also makes the root give way
        assert_eq!(at(450.0).0, true);
    }

    #[test]
    fn a_stand_in_gives_way_only_when_all_its_children_are_there() {
        // the parent says it has 3 children, 2 are listed; a third is in a map that is not streamed in
        let mut ents = vec![lod_ent(0.0, 1, 500.0, 300.0, 3, worldload::NO_PARENT), lod_ent(1.0, 0, 100.0, 0.0, 0, 0), lod_ent(2.0, 0, 100.0, 0.0, 0, 0)];
        let vp = Mat4::IDENTITY;
        let data = WorldData::assemble(vec![], vec![], ents.clone(), HashMap::new(), vec![]);
        assert!(!expands_at(&lod_view(10.0, &data, &vp, &[]), &data, &mut vec![], 0), "two of three children");
        ents[0].num_children = 2;
        ents[2].map = 1;
        let data = WorldData::assemble(vec!["a".into(), "b".into()], vec![], ents, HashMap::new(), vec![]);
        assert!(expands_at(&lod_view(10.0, &data, &vp, &[]), &data, &mut vec![], 0), "both children there");
        assert!(expands_at(&lod_view(10.0, &data, &vp, &[true, true]), &data, &mut vec![], 0), "both maps streamed in");
        assert!(!expands_at(&lod_view(10.0, &data, &vp, &[true, false]), &data, &mut vec![], 0), "a child's map is not streamed in");
    }

    #[test]
    fn a_point_straight_ahead_is_the_centre_and_one_behind_is_not_shown() {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::ZERO;
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        let vp = cam.view_proj(800.0 / 600.0);
        let p = project(&vp, Vec3::new(10.0, 0.0, 0.0), rect()).unwrap();
        assert!((p.x - 400.0).abs() < 0.5 && (p.y - 300.0).abs() < 0.5, "{p:?}");
        assert!(project(&vp, Vec3::new(-10.0, 0.0, 0.0), rect()).is_none());
        // up in the world is up on the screen, to the camera's right is right
        let up = project(&vp, Vec3::new(10.0, 0.0, 2.0), rect()).unwrap();
        assert!(up.y < 300.0);
        let right = project(&vp, Vec3::new(10.0, -2.0, 0.0), rect()).unwrap();
        assert!(right.x > 400.0, "looking along +X, +Y is left and -Y is right: {right:?}");
    }

    #[test]
    fn a_line_through_the_camera_plane_is_cut() {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::ZERO;
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        let vp = cam.view_proj(800.0 / 600.0);
        assert!(project_segment(&vp, Vec3::new(-5.0, 1.0, 0.0), Vec3::new(20.0, 1.0, 0.0), rect()).is_some());
        assert!(project_segment(&vp, Vec3::new(-5.0, 1.0, 0.0), Vec3::new(-1.0, 1.0, 0.0), rect()).is_none());
    }

    #[test]
    fn flying_moves_along_the_view() {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::ZERO;
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        cam.speed = 10.0;
        cam.fly(Vec3::new(0.0, 1.0, 0.0), 1.0, false);
        assert!((cam.pos - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);
        cam.fly(Vec3::new(1.0, 0.0, 0.0), 1.0, true);
        assert!((cam.pos.y + 40.0).abs() < 1e-3, "right of +X is -Y: {:?}", cam.pos);
        cam.fly(Vec3::new(0.0, 0.0, 1.0), 0.5, false);
        assert!((cam.pos.z - 5.0).abs() < 1e-4);
    }

    #[test]
    fn a_box_has_12_distinct_edges_of_the_right_length() {
        let edges = box_edges(Vec3::new(1.0, 2.0, 3.0), Quat::from_rotation_z(0.7), Vec3::new(1.0, 2.0, 3.0));
        let mut lens: Vec<f32> = edges.iter().map(|(a, b)| (*a - *b).length()).collect();
        lens.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(lens.iter().filter(|l| (**l - 2.0).abs() < 1e-4).count(), 4);
        assert_eq!(lens.iter().filter(|l| (**l - 4.0).abs() < 1e-4).count(), 4);
        assert_eq!(lens.iter().filter(|l| (**l - 6.0).abs() < 1e-4).count(), 4);
    }

    #[test]
    fn a_ray_hits_a_box_in_front_and_misses_one_to_the_side() {
        let (o, d) = (Vec3::ZERO, Vec3::X);
        let t = ray_box(o, d, Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::splat(1.0)).unwrap();
        assert!((t - 9.0).abs() < 1e-4, "{t}");
        assert!(ray_box(o, d, Vec3::new(10.0, 5.0, 0.0), Quat::IDENTITY, Vec3::splat(1.0)).is_none());
        // a long thin box turned a quarter round now points along Y: the ray along X crosses its short side
        let r = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let t = ray_box(o, d, Vec3::new(10.0, 0.0, 0.0), r, Vec3::new(4.0, 1.0, 1.0)).unwrap();
        assert!((t - 9.0).abs() < 1e-3, "{t}");
        assert!(ray_box(o, d, Vec3::new(-10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::splat(1.0)).is_none(), "behind the camera");
    }

    #[test]
    fn boxes_outside_the_view_are_culled_and_the_ones_in_it_or_touching_it_are_not() {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::ZERO;
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        let vp = cam.view_proj(800.0 / 600.0);
        let planes = frustum_planes(&vp);
        let b = |x: f32, y: f32, z: f32, s: f32, pad: f32| box_outside(&planes, Vec3::new(x - s, y - s, z - s), Vec3::new(x + s, y + s, z + s), pad);
        assert!(!b(100.0, 0.0, 0.0, 10.0, 0.0), "straight ahead");
        assert!(b(-100.0, 0.0, 0.0, 10.0, 0.0), "behind the camera");
        assert!(b(100.0, 500.0, 0.0, 10.0, 0.0), "far to the side");
        assert!(!b(100.0, 500.0, 0.0, 10.0, 600.0), "a model that reaches 600 m can still come into view");
        assert!(b(100.0, 0.0, 500.0, 10.0, 0.0), "far above");
        assert!(!box_outside(&planes, Vec3::splat(-128.0), Vec3::splat(128.0), 0.0), "the cell the camera is in is never culled");
        // every point the projection puts on the screen is in a box the planes keep
        let r = rect();
        let mut checked = 0;
        for xi in 1..40 {
            for yi in -20..20 {
                for zi in -12..12 {
                    let p = Vec3::new(xi as f32 * 25.0, yi as f32 * 25.0, zi as f32 * 25.0);
                    if project(&vp, p, r).map_or(false, |s| r.contains(s)) {
                        checked += 1;
                        assert!(!box_outside(&planes, p - Vec3::splat(0.01), p + Vec3::splat(0.01), 0.0), "{p:?} is on the screen");
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked}");
    }

    fn pick(id: u32, x: f32, y: f32) -> Pick {
        Pick { id, c3: Vec3::new(x, y, 0.0), rot: Quat::IDENTITY, half: Vec3::splat(1.0) }
    }

    #[test]
    fn picking_takes_the_nearest_box_along_the_ray_and_ignores_misses_and_boxes_behind() {
        let picks = vec![pick(1, 20.0, 0.0), pick(2, 10.0, 0.0), pick(3, 10.0, 5.0), pick(4, -10.0, 0.0)];
        assert_eq!(pick_ray(&picks, Vec3::ZERO, Vec3::X), Some(2), "the nearer of two in line");
        assert_eq!(pick_ray(&picks, Vec3::ZERO, Vec3::Y), None);
        assert_eq!(pick_ray(&picks, Vec3::ZERO, -Vec3::X), Some(4), "behind is in front when looking the other way");
        assert_eq!(pick_ray(&[], Vec3::ZERO, Vec3::X), None);
        // a ray that starts inside a box still hits it
        assert_eq!(pick_ray(&[pick(9, 0.0, 0.0)], Vec3::ZERO, Vec3::X), Some(9));
    }

    #[test]
    fn a_box_selection_takes_what_is_inside_the_rectangle_on_the_screen() {
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::ZERO;
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        let vp = cam.view_proj(800.0 / 600.0);
        // looking along +X: -Y is right on the screen
        let picks = vec![pick(1, 20.0, 0.0), pick(2, 20.0, -6.0), pick(3, 20.0, 6.0), pick(4, -20.0, 0.0)];
        let whole = picks_in_rect(&picks, &vp, rect(), rect());
        assert_eq!(whole, vec![1, 2, 3], "the one behind the camera is never in");
        let right_half = picks_in_rect(&picks, &vp, rect(), egui::Rect::from_min_max(Pos2::new(401.0, 0.0), Pos2::new(800.0, 600.0)));
        assert_eq!(right_half, vec![2]);
        let none = picks_in_rect(&picks, &vp, rect(), egui::Rect::from_min_size(Pos2::new(0.0, 0.0), egui::vec2(5.0, 5.0)));
        assert!(none.is_empty());
    }

    #[test]
    fn an_entity_box_follows_scale_and_the_inverse_stored_rotation() {
        let ed = EntityEdit { pos: [10.0, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 2.0, scale_z: 3.0 };
        let (c, rot, half) = entity_box_of(Some(([-1.0, -1.0, 0.0], [1.0, 1.0, 2.0])), &ed);
        assert!((c - Vec3::new(10.0, 0.0, 3.0)).length() < 1e-5, "{c:?}");
        assert!(rot.angle_between(Quat::IDENTITY) < 1e-5);
        assert!((half - Vec3::new(2.0, 2.0, 3.0)).length() < 1e-5, "{half:?}");
        // an entity with no known archetype is a 2 m cube
        let (_, _, unit) = entity_box_of(None, &ed);
        assert_eq!(unit, Vec3::ONE);
        // a stored quarter turn about Z is a world turn the other way
        let s = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let turned = EntityEdit { rot: [s.x, s.y, s.z, s.w], ..ed };
        let (_, r, _) = entity_box_of(None, &turned);
        assert!((r * Vec3::X - (-Vec3::Y)).length() < 1e-4, "{:?}", r * Vec3::X);
    }
}
