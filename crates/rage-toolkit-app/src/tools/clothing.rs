//! Clothing Pack Creator: drop .ydd models and .ytd textures (files or whole folders), see them paired and grouped by component,
//! fix the numbering, check them, preview what will be written, and build the FiveM resource.
//! The work is `rage_toolkit_core::clothing`; this page only shows it and asks the user.
use super::*;
use eframe::egui::Color32;
use rage_toolkit_core::clothing::{self, build, game_dlc, project as cproj, Drawable, Kind, OnConflict, Project, Sev, Sex, Texture};
use std::collections::BTreeSet;

type Key = clothing::Slot;

/// The game's clothing DLCs and its file index (the slots are named as the game's files).
pub struct Rockstar {
    dlcs: Vec<game_dlc::GameDlc>,
    index: rage_toolkit_core::index::FileIndex,
}

/// What making DLC `dlc` Rockstar DLC `game` needs, read from the game.
pub struct Convert {
    dlc: usize,
    game: game_dlc::GameDlc,
    files: std::collections::HashMap<String, Vec<u8>>,
    races: [game_dlc::Races; 2],
}

/// One open clothing pack (a tab of the page).
pub struct Pack {
    project: Project,
    /// where the project was saved or opened from
    file: Option<PathBuf>,
    dirty: bool,
    selected: BTreeSet<Key>,
    anchor: Option<Key>,
    /// the sex for dropped files whose path does not say
    default_sex: Sex,
    /// what was just dropped and clashes with numbers in use: waiting for the user's choice
    pending: Option<clothing::ImportPlan>,
    pending_note: String,
    /// drawables put aside for a second addon
    held: Vec<Drawable>,
    notes: Vec<String>,
    plan: Option<build::BuildPlan>,
    out_dir: String,
    message: Option<Result<String, String>>,
    /// counts every change of the project: the checks and the build plan are made again (on a thread) when it moves
    rev: u64,
    /// the checks of the project as of `checked_rev` (made on a thread; the page shows the last ones meanwhile)
    issues: std::rc::Rc<Vec<clothing::Issue>>,
    checked_rev: u64,
    checked_at: std::time::Instant,
    check_task: Task<(u64, Vec<clothing::Issue>)>,
    /// the build plan as of `plan_rev`, made on a thread while the Build window is open
    plan_rev: u64,
    plan_task: Task<(u64, build::BuildPlan)>,
    /// the game's clothing DLCs (listed once, when the Build window first opens with the game set), for making a DLC a Rockstar one
    rockstar: Option<std::sync::Arc<Rockstar>>,
    rockstar_task: Task<std::sync::Arc<Rockstar>>,
    rockstar_error: Option<String>,
    /// a DLC being made a Rockstar one: the game's files are read on a thread, then the change is made here
    convert_task: Task<Convert>,
    /// the 3D preview: on or off, the texture shown (index into the drawable's textures), the body around it
    preview_on: bool,
    preview: super::cloth_preview::Preview,
    preview_tex: usize,
    preview_body: bool,
    preview_lod: rage_toolkit_core::model::LodLevel,
    /// the drawables list's search box
    search: String,
    checks_open: bool,
    build_open: bool,
    /// build over an earlier build: only what changed is written
    update_build: bool,
    /// the build and the texture estimate run on another thread, with progress
    build_task: Task<String>,
    build_job: rage_toolkit_core::job::Job,
    size_task: Task<String>,
    size_job: rage_toolkit_core::job::Job,
    size_text: Option<String>,
    /// a removed (archived) drawable shown in the card instead of the selection
    archived_sel: Option<usize>,
    /// the Move to slot boxes: DLC, sex, kind, number (filled from the selected drawable)
    slot: Option<(Key, Key)>,
    confirm_delete: bool,
    /// the other open packs (index, name), for Copy to / Move to; set by the page every frame
    others: Vec<(usize, String)>,
    /// drawables to add to another pack (its index): done by the page after this pack is drawn
    outbox: Vec<(Drawable, usize)>,
    /// what is known of texture files and models (size, format), read when a drawable is shown
    tex_info: std::collections::HashMap<PathBuf, Option<Vec<TexInfo>>>,
    /// folded groups of the list (DLC, sex, type)
    folded: std::collections::HashSet<String>,
    layout_open: bool,
}

/// One texture as a file holds it: for a `.ytd` its textures, for a model its embedded ones.
#[derive(Debug, Clone)]
struct TexInfo {
    name: String,
    width: u32,
    height: u32,
    format: String,
    /// pixel data with every mip level: what the game holds in memory once it is streamed in
    bytes: usize,
    /// the size of the file it is in, on disk (compressed; for a model, textures and geometry together)
    file: u64,
}

fn read_tex_info(p: &Path) -> Option<Vec<TexInfo>> {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let file = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    match ext.as_str() {
        "ytd" => rage_toolkit_core::pipeline::read_ytd(p).ok().map(|(ts, _)| ts.iter().map(|t| TexInfo { name: t.name.clone(), width: t.width as u32, height: t.height as u32, format: t.format.to_string(), bytes: t.pixel_data.len(), file }).collect()),
        "ydd" | "ydr" => rage_toolkit_core::yddtools::report(p).ok().map(|rows| rows.into_iter().map(|r| TexInfo { name: r.name, width: r.width, height: r.height, format: r.format.to_string(), bytes: r.bytes, file }).collect()),
        _ => None,
    }
}

fn mb(b: usize) -> String {
    if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / 1_048_576.0)
    } else {
        format!("{} KB", b.div_ceil(1024))
    }
}

const MEMORY_TIP: &str = "In memory: the pixel data with every mip level, what the game holds once the texture is streamed in (what counts against streaming memory). File: the size on disk, which is compressed and usually much smaller.";

/// The two dropdowns of a texture change: the longest side and the format ("keep" is the file as it is). True when changed.
fn edit_controls(ui: &mut egui::Ui, id: &str, e: &mut clothing::TexEdit, info: Option<&TexInfo>) -> bool {
    let mut changed = false;
    let side = info.map(|t| t.width.max(t.height)).unwrap_or(0);
    let format = info.map(|t| t.format.clone()).unwrap_or_default();
    let size_text = match e.max {
        Some(m) => format!("{m} px"),
        None if side > 0 => format!("{side} px"),
        None => "size".into(),
    };
    egui::ComboBox::from_id_salt((id, "size")).selected_text(size_text).width(78.0).show_ui(ui, |ui| {
        changed |= ui.selectable_value(&mut e.max, None, if side > 0 { format!("keep ({side} px)") } else { "keep".into() }).changed();
        for v in [4096u32, 2048, 1024, 512, 256, 128, 64, 32] {
            if side == 0 || v < side {
                changed |= ui.selectable_value(&mut e.max, Some(v), format!("{v} px")).changed();
            }
        }
    }).response.on_hover_text("The longest side in the built pack (never larger than it is)");
    let fmt_text = match &e.format {
        Some(f) => f.to_uppercase(),
        None if !format.is_empty() => format.clone(),
        None => "format".into(),
    };
    egui::ComboBox::from_id_salt((id, "fmt")).selected_text(fmt_text).width(92.0).show_ui(ui, |ui| {
        changed |= ui.selectable_value(&mut e.format, None, if format.is_empty() { "keep".to_string() } else { format!("keep ({format})") }).changed();
        for (f, what) in clothing::TEX_FORMATS {
            if !f.eq_ignore_ascii_case(&format) {
                changed |= ui.selectable_value(&mut e.format, Some(f.to_string()), f.to_uppercase()).on_hover_text(what).changed();
            }
        }
    }).response.on_hover_text("The format in the built pack");
    changed
}

/// what a change does, as a short line ("-> 512 px DXT1")
fn edit_note(e: &clothing::TexEdit) -> String {
    let mut v = vec![];
    if let Some(m) = e.max {
        v.push(format!("{m} px"));
    }
    if let Some(f) = &e.format {
        v.push(f.to_uppercase());
    }
    format!("-> {} in the build", v.join(" "))
}

/// A titled section of the drawable card.
fn section(ui: &mut egui::Ui, title: &str, note: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    egui::Frame::new().fill(crate::theme::surface()).stroke(egui::Stroke::new(1.0, crate::theme::border())).corner_radius(8.0).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(title).size(15.0).strong());
            if !note.is_empty() {
                ui.label(egui::RichText::new(note).small().color(crate::theme::dim()));
            }
        });
        ui.add_space(4.0);
        add(ui);
    });
}

impl Default for Pack {
    fn default() -> Self {
        Self { project: Project::default(), file: None, dirty: false, selected: BTreeSet::new(), anchor: None, default_sex: Sex::Male, pending: None, pending_note: String::new(), held: vec![], notes: vec![], plan: None, out_dir: String::new(), message: None, rev: 0, issues: Default::default(), checked_rev: u64::MAX, checked_at: std::time::Instant::now(), check_task: Task::default(), plan_rev: u64::MAX, plan_task: Task::default(), rockstar: None, rockstar_task: Task::default(), rockstar_error: None, convert_task: Task::default(), preview_on: true, preview: Default::default(), preview_tex: 0, preview_body: true, preview_lod: rage_toolkit_core::model::LodLevel::High, search: String::new(), checks_open: false, build_open: false, update_build: false, build_task: Task::default(), build_job: Default::default(), size_task: Task::default(), size_job: Default::default(), size_text: None, archived_sel: None, slot: None, confirm_delete: false, others: vec![], outbox: vec![], tex_info: Default::default(), folded: Default::default(), layout_open: false }
    }
}

/// a file name without a built pack's `mp_f_freemode_01_pack^` prefix
fn short(p: &Path) -> String {
    let f = clothing::file_of(p);
    f.rsplit('^').next().unwrap_or(&f).to_string()
}

fn key(d: &Drawable) -> Key {
    d.slot()
}

fn sev_colour(s: Sev) -> Color32 {
    match s {
        Sev::Error => crate::theme::err(),
        Sev::Warning => crate::theme::warn(),
        Sev::Info => crate::theme::info(),
    }
}

impl Pack {
    /// the 3D preview is still loading (automated screenshots wait for it)
    pub fn busy(&self) -> bool {
        (self.preview_on && self.preview.loading()) || self.build_task.busy() || self.size_task.busy()
    }

    /// The project's checks (see [`Checked`]).
    /// The last checks of the project (they are made on a thread, see `background`; a change shows in them a moment later).
    fn issues(&mut self) -> std::rc::Rc<Vec<clothing::Issue>> {
        self.issues.clone()
    }

    /// whether the checks shown are for the project as it is now
    fn checks_current(&self) -> bool {
        self.checked_rev == self.rev
    }

    /// The checks and the build plan, made on threads so the page never waits for them: the checks when the project changed
    /// (and every 15 seconds, for files changed on disk), the plan when it changed and the Build window is open.
    fn background(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.check_task.poll(ctx) {
            match r {
                Ok((rev, v)) => {
                    self.issues = std::rc::Rc::new(v);
                    self.checked_rev = rev;
                }
                Err(e) => self.message = Some(Err(format!("the checks stopped: {e}"))),
            }
            self.checked_at = std::time::Instant::now();
        }
        let stale = self.checked_rev != self.rev || self.checked_at.elapsed() > std::time::Duration::from_secs(15);
        if stale && !self.check_task.busy() {
            let (p, rev) = (self.project.clone(), self.rev);
            self.checked_at = std::time::Instant::now();
            self.check_task.run(move || Ok((rev, clothing::validate(&p))));
        }
        if let Some(r) = self.plan_task.poll(ctx) {
            match r {
                Ok((rev, plan)) => {
                    self.plan = Some(plan);
                    self.plan_rev = rev;
                }
                Err(e) => self.message = Some(Err(format!("planning the build stopped: {e}"))),
            }
        }
        if self.build_open && self.plan_rev != self.rev && !self.plan_task.busy() {
            let (p, rev) = (self.project.clone(), self.rev);
            self.plan_task.run(move || Ok((rev, build::plan_build(&p))));
        }
    }

    /// Files and folders dropped on the window while this page is open.
    pub fn add_dropped(&mut self, paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        // models picked alone bring the textures and cloth of their number from their folder
        let scan = clothing::scan(&clothing::with_textures(paths));
        let paired = clothing::pair(&scan, self.default_sex);
        self.notes.clear();
        if !scan.ignored.is_empty() {
            self.notes.push(format!("{} file(s) do not have clothing names and were left out, for example {}", scan.ignored.len(), clothing::file_of(&scan.ignored[0])));
        }
        if !paired.orphan_textures.is_empty() {
            self.notes.push(format!("{} texture(s) have no model with that number (for example {}); they were not added", paired.orphan_textures.len(), clothing::file_of(&paired.orphan_textures[0].path)));
        }
        if !paired.duplicate_drawables.is_empty() {
            self.notes.push(format!("{} model(s) share a number with another (for example {}): all are kept; choose below where the extra ones go", paired.duplicate_drawables.len(), clothing::file_of(&paired.duplicate_drawables[0].path)));
        }
        if paired.drawables.is_empty() {
            self.message = Some(Err("no clothing models found in what was dropped (jbib_000_u.ydd, p_head_000.ydd, ...)".into()));
            return true;
        }
        let plan = clothing::plan_import(&self.project, paired.drawables);
        if plan.conflicts.is_empty() {
            let n = plan.fresh.len();
            clothing::apply_import(&mut self.project, plan, OnConflict::Skip);
            self.message = Some(Ok(format!("Added {n} drawable(s).")));
            self.dirty = true;
            self.rev += 1;
        } else {
            // never renumber silently: ask
            self.pending_note = format!("{} dropped drawable(s) use numbers that are already in the project ({} others fit).", plan.conflicts.len(), plan.fresh.len());
            self.pending = Some(plan);
        }
        self.plan = None;
        // something to look at straight away
        if self.selected.is_empty() {
            if let Some(d) = self.project.drawables.first() {
                self.selected.insert(key(d));
                self.anchor = Some(key(d));
            }
        }
        true
    }

    /// The page, laid out like other clothing tools: the project bar on top, the drawables on the left, the selected drawable in the
    /// middle, the 3D preview on the right, the actions at the bottom; the checks and the build open in their own windows.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, render: Option<&eframe::egui_wgpu::RenderState>, game: Option<&Game>) {
        self.background(ctx);
        self.project_bar(ui);
        if self.pending.is_some() {
            self.conflict_bar(ui);
        }
        // an opened pack can bring hundreds: the first few here, all of them in a list that opens
        let shown = if self.notes.len() > 6 { 4 } else { self.notes.len() };
        for n in &self.notes[..shown] {
            ui.label(egui::RichText::new(n).color(crate::theme::warn()).small());
        }
        if self.notes.len() > shown {
            egui::CollapsingHeader::new(egui::RichText::new(format!("{} more note(s)", self.notes.len() - shown)).small()).id_salt("cl_notes").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for n in &self.notes[shown..] {
                        ui.label(egui::RichText::new(n).color(crate::theme::warn()).small());
                    }
                });
            });
        }
        if !self.held.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("{} drawable(s) are held back for a second addon.", self.held.len())).color(crate::theme::info()));
                if ui.button("Start a new project with them").on_hover_text("Replaces this project: save it first").clicked() {
                    let held = std::mem::take(&mut self.held);
                    let name = format!("{}_02", self.project.name);
                    self.project = Project { name, limit: self.project.limit, drawables: held.into_iter().map(|d| Drawable { dlc: 0, ..d }).collect(), ..Default::default() };
                    self.project.close_gaps();
                    self.file = None;
                    self.selected.clear();
                    self.dirty = true;
            self.rev += 1;
                    self.plan = None;
                }
                if ui.button("Forget them").clicked() {
                    self.held.clear();
                }
            });
        }
        egui::Panel::bottom("cl_bottom").frame(crate::theme::panel_frame()).show(ui, |ui| self.bottom_bar(ui));
        egui::Panel::left("cl_list").default_size(250.0).resizable(true).frame(crate::theme::panel_frame()).show(ui, |ui| self.list(ui));
        if self.preview_on {
            egui::Panel::right("cl_preview").default_size(340.0).resizable(true).frame(crate::theme::panel_frame()).show(ui, |ui| self.preview_panel(ui, render, game));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("cl_details").auto_shrink([false, false]).show(ui, |ui| self.details(ui));
        });
        self.checks_window(ctx);
        self.build_window(ctx, game);
        self.layout_window(ctx);
    }

    /// The pack's organisation: DLCs (names, counts, creature metadata), the resources a build makes and what each holds, the
    /// files carried as they are, and the layout settings.
    fn layout_window(&mut self, ctx: &egui::Context) {
        if !self.layout_open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new(format!("Pack layout: {}", self.project.name)).open(&mut open).default_size([760.0, 560.0]).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let p = &mut self.project;
                let split = p.layout == clothing::Layout::ControlAndStreams;
                section(ui, "Layout", "how a build lays the pack out (also in the Build window)", |ui| {
                    ui.horizontal(|ui| {
                        changed |= ui.radio_value(&mut p.layout, clothing::Layout::Single, "One resource").changed();
                        changed |= ui.radio_value(&mut p.layout, clothing::Layout::ControlAndStreams, "Control + stream resources").changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("A DLC holds at most");
                        egui::ComboBox::from_id_salt("cl_lay_limit").selected_text(format!("{} of a type", p.limit)).show_ui(ui, |ui| {
                            for v in [128usize, 256] {
                                changed |= ui.selectable_value(&mut p.limit, v, format!("{v} (game builds {})", if v == 128 { "before 2944" } else { "2944 and later" })).changed();
                            }
                        });
                        if p.layout == clothing::Layout::ControlAndStreams {
                            ui.label("; a stream resource at most");
                            changed |= ui.add(egui::DragValue::new(&mut p.stream_limit).range(1..=1000).suffix(" of a type")).changed();
                        }
                    });
                    ui.label(egui::RichText::new(if split {
                        format!("{0}_control holds the manifest, the YMTs, shop metas, creature metadata, alternate variations and first-person files. {0}_stream_m1, _f1, _m2, ... (sex and DLC number) only stream the models and textures, at most {1} of a type each: the rest go to _m1_02, _m1_03, ... Rockstar's DLCs are streamed by {0}_stream_base.", p.name, p.stream_limit)
                    } else {
                        format!("One resource, {}: the manifest and metas at its top, the YMTs and every model and texture in its stream folder.", p.name)
                    }).small().color(crate::theme::dim()));
                });

                // the resources a build makes
                let mut per: std::collections::BTreeMap<String, std::collections::BTreeMap<(Sex, Kind), usize>> = Default::default();
                for d in p.drawables.iter().filter(|d| !d.ydd.as_os_str().is_empty()) {
                    let r = if split { build::stream_resource(p, d) } else { p.name.clone() };
                    *per.entry(r).or_default().entry((d.sex, d.kind)).or_default() += 1;
                }
                let control_files = (0..p.dlcs.len()).filter(|k| p.drawables.iter().any(|d| d.dlc == *k)).count();
                section(ui, "Resources a build makes", "and how many drawables of each type each one streams", |ui| {
                    if split {
                        ui.label(egui::RichText::new(format!("{}_control", p.name)).strong());
                        ui.label(egui::RichText::new(format!("   the YMTs and metas of {control_files} DLC(s), creature metadata, alternate variations, first-person files{}", if p.carried.iter().any(|c| c.place == clothing::Place::Control) { ", and files carried as they are" } else { "" })).small().color(crate::theme::dim()));
                    }
                    for (r, kinds) in &per {
                        let over = kinds.values().any(|n| *n > p.stream_limit) && split;
                        ui.label(egui::RichText::new(r).strong().color(if over { crate::theme::err() } else { crate::theme::text() }));
                        let line: Vec<String> = kinds.iter().map(|((sex, kind), n)| format!("{} {kind_name} {n}", if sex == &Sex::Male { "m" } else { "f" }, kind_name = kind.name())).collect();
                        ui.label(egui::RichText::new(format!("   {}", line.join(", "))).small().color(crate::theme::dim()));
                    }
                    let carried_base = p.carried.iter().filter(|c| c.place != clothing::Place::Control).count();
                    if carried_base > 0 && split {
                        ui.label(egui::RichText::new(format!("   and {carried_base} file(s) carried as they are into the stream resources (replacements of the game's files, extras)")).small().color(crate::theme::dim()));
                    }
                    if per.is_empty() {
                        ui.weak("No drawables yet.");
                    }
                });

                section(ui, "DLCs", "the game finds a drawable by its DLC name and its number in it", |ui| {
                    egui::Grid::new("cl_lay_dlcs").num_columns(6).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                        for h in ["#", "male name", "female name", "male", "female", "creature metadata"] {
                            ui.label(egui::RichText::new(h).small().color(crate::theme::dim()));
                        }
                        ui.end_row();
                        for k in 0..p.dlcs.len() {
                            let (m, f) = (p.drawables.iter().filter(|d| d.dlc == k && d.sex == Sex::Male).count(), p.drawables.iter().filter(|d| d.dlc == k && d.sex == Sex::Female).count());
                            ui.label(format!("{}{}", k + 1, if p.dlcs[k].base { " (Rockstar's)" } else { "" }));
                            let (gm, gf) = (p.dlc_name(k, Sex::Male), p.dlc_name(k, Sex::Female));
                            match &mut p.dlcs[k].names {
                                Some((mn, fnm)) => {
                                    changed |= ui.add(egui::TextEdit::singleline(mn).desired_width(150.0)).on_hover_text("Renaming a DLC of a pack in use changes where the game looks for its drawables").changed();
                                    changed |= ui.add(egui::TextEdit::singleline(fnm).desired_width(150.0)).changed();
                                }
                                None => {
                                    ui.label(egui::RichText::new(gm).color(crate::theme::dim())).on_hover_text("Named after the project (<name>_<number>)");
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(&gf).color(crate::theme::dim()));
                                        if ui.small_button("name it").on_hover_text("Give this DLC names of its own instead of <project>_<number>").clicked() {
                                            p.dlcs[k].names = Some((p.dlc_name(k, Sex::Male), gf.clone()));
                                            changed = true;
                                        }
                                    });
                                }
                            }
                            ui.label(m.to_string());
                            ui.label(f.to_string());
                            let feet_hats = p.drawables.iter().any(|d| d.dlc == k && matches!(d.kind, Kind::Component(6) | Kind::Prop(0)));
                            ui.label(egui::RichText::new(format!("{}{}", p.creature_name(k), if feet_hats { "" } else { " (none written: no feet or hats)" })).small());
                            ui.end_row();
                        }
                    });
                });

                if !p.carried.is_empty() {
                    section(ui, &format!("Carried as they are ({})", p.carried.len()), "files of an opened pack the builder does not model; written into the build unchanged", |ui| {
                        egui::ScrollArea::vertical().id_salt("cl_lay_carried").max_height(160.0).show(ui, |ui| {
                            for c in &p.carried {
                                let place = match &c.place {
                                    clothing::Place::Control => "control".to_string(),
                                    clothing::Place::BaseStream => "stream_base".to_string(),
                                    clothing::Place::Stream(x) => x.clone(),
                                };
                                ui.label(egui::RichText::new(format!("{place}: {}{}", c.dest, c.data_file.as_ref().map(|k| format!("  ({k})")).unwrap_or_default())).small().monospace());
                            }
                        });
                    });
                }
            });
        });
        if changed {
            self.dirty = true;
            self.rev += 1;
            self.plan = None;
        }
        self.layout_open = open;
    }

    /// Bottom: add female / male files or folders, the actions on the selection, the checks, the 3D preview, Build.
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for sex in [Sex::Female, Sex::Male] {
                ui.menu_button(egui::RichText::new(format!("+ {}", sex.label().to_uppercase())).strong(), |ui| {
                    ui.weak(format!("Files whose folder does not say male or female count as {}.", sex.label()));
                    if ui.button("Models (.ydd)...").on_hover_text("Pick the .ydd models: the textures (.ytd or images) and cloth (.yld) of the same number in their folder come along").clicked() {
                        ui.close();
                        if let Some(fs) = rfd::FileDialog::new().add_filter("Clothing models", &["ydd"]).pick_files() {
                            self.default_sex = sex;
                            self.add_dropped(&fs);
                        }
                    }
                    if ui.button("A folder...").clicked() {
                        ui.close();
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            self.default_sex = sex;
                            self.add_dropped(&[d]);
                        }
                    }
                });
            }
            let any = !self.selected.is_empty();
            if ui.add_enabled(any, egui::Button::new("Remove")).on_hover_text("Move the selected drawables to Removed (kept with their files: restore them, move them to another pack, or delete them there)").clicked() {
                self.archive_selected();
            }
            ui.separator();
            if ui.add_enabled(any, egui::Button::new("Move up")).on_hover_text("Swap numbers with the one before").clicked() {
                self.move_selected(-1);
            }
            if ui.add_enabled(any, egui::Button::new("Move down")).clicked() {
                self.move_selected(1);
            }
            let has = !self.project.drawables.is_empty();
            if ui.add_enabled(has, egui::Button::new("Close gaps")).on_hover_text("Number every component 000, 001, 002, ... in the current order").clicked() {
                self.project.close_gaps();
                self.selected.clear();
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
            }
            if ui.add_enabled(has, egui::Button::new("Re-letter")).on_hover_text("a, b, c, ... in the current order").clicked() {
                self.project.relabel_textures();
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
            }
            // only when a DLC is over its limit: the drawables past it go to the next DLC (a new one when needed)
            if self.issues().iter().any(|i| i.code == "CLOTH_LIMIT") && ui.button("Move overflow to next DLC").on_hover_text(format!("The drawables past number {} of a DLC move to the next DLC, numbered after its last; nothing else changes", self.project.limit)).clicked() {
                let n = self.project.split_full_dlcs();
                self.selected.clear();
                self.changed(format!("{n} drawable(s) moved to the next DLC"));
            }
        });
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 34.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            {
                let big = |t: &str| egui::Button::new(egui::RichText::new(t).strong().size(14.0)).min_size(egui::vec2(110.0, 30.0));
                if ui.add(big("BUILD")).on_hover_text("Write the FiveM resource: the YMT, the shop meta, the manifest and the streamed files").clicked() {
                    self.build_open = true;
                }
                ui.toggle_value(&mut self.preview_on, egui::RichText::new("3D PREVIEW").strong().size(14.0)).on_hover_text("Show the selected drawable on the freemode body, with the texture chosen in its list");
                ui.toggle_value(&mut self.layout_open, egui::RichText::new("PACK LAYOUT").strong().size(14.0)).on_hover_text("How the pack is organised: its DLCs, the resources a build makes and what goes in each, the files carried as they are");
                let issues = self.issues();
                let (e, w) = (issues.iter().filter(|i| i.sev == Sev::Error).count(), issues.iter().filter(|i| i.sev == Sev::Warning).count());
                let (mut text, colour) = if e + w == 0 { ("no problems".to_string(), crate::theme::ok()) } else { (format!("{e} error(s), {w} warning(s)"), if e > 0 { crate::theme::err() } else { crate::theme::warn() }) };
                if !self.checks_current() {
                    text = format!("checking... ({text})");
                }
                if ui.add(egui::Button::new(egui::RichText::new(text).color(colour))).on_hover_text("Every check of the project, with how to fix it").clicked() {
                    self.checks_open = true;
                }
            }
        });
        if let Some(m) = &self.message {
            match m {
                Ok(t) => ui.label(egui::RichText::new(t).color(crate::theme::ok())),
                Err(t) => ui.label(egui::RichText::new(t).color(crate::theme::err())),
            };
        }
    }

    fn project_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Pack name");
            if ui.add(egui::TextEdit::singleline(&mut self.project.name).desired_width(140.0)).changed() {
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
            }
            ui.label("per addon");
            let mut limit = self.project.limit;
            egui::ComboBox::from_id_salt("cl_limit").selected_text(format!("{limit} drawables")).width(120.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut limit, 128, "128 (older game builds)");
                ui.selectable_value(&mut limit, 256, "256 (newer game builds)");
            });
            if limit != self.project.limit {
                self.project.limit = limit;
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
            }
            ui.separator();
            if ui.button("New").clicked() {
                *self = Pack::default();
            }
            if ui.button("Open...").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("Clothing project", &["ragetoolkitclothes"]).pick_file() {
                    match cproj::load(&f) {
                        Ok(p) => {
                            self.project = p;
                            self.file = Some(f);
                            self.dirty = false;
                            self.rev += 1;
                            self.selected.clear();
                            self.plan = None;
                            self.message = Some(Ok("Project opened.".into()));
                        }
                        Err(e) => self.message = Some(Err(format!("{e:#}"))),
                    }
                }
            }
            if ui.add_enabled(self.file.is_some() && self.dirty, egui::Button::new("Save")).clicked() {
                self.save(false);
            }
            if ui.button("Save as...").clicked() {
                self.save(true);
            }
            if self.dirty {
                ui.label(egui::RichText::new("unsaved changes").color(crate::theme::warn()).small());
            }
            if let Some(f) = &self.file {
                ui.weak(f.display().to_string());
            }
        });
    }

    fn save(&mut self, ask: bool) {
        let file = if ask || self.file.is_none() {
            rfd::FileDialog::new().set_file_name(format!("{}.ragetoolkitclothes", self.project.name)).add_filter("Clothing project", &["ragetoolkitclothes"]).save_file()
        } else {
            self.file.clone()
        };
        if let Some(f) = file {
            match cproj::save(&self.project, &f) {
                Ok(()) => {
                    self.file = Some(f);
                    self.dirty = false;
                    self.message = Some(Ok("Project saved.".into()));
                }
                Err(e) => self.message = Some(Err(format!("{e:#}"))),
            }
        }
    }

    fn conflict_bar(&mut self, ui: &mut egui::Ui) {
        let mut choice: Option<OnConflict> = None;
        let mut cancel = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new(&self.pending_note).color(crate::theme::warn()).strong());
            ui.weak("Nothing is renumbered unless you say so. What should happen to them?");
            ui.horizontal_wrapped(|ui| {
                if ui.button("Continue numbering after the last one").on_hover_text("They get the next free numbers; their file names change").clicked() {
                    choice = Some(OnConflict::Continue);
                }
                if ui.button("Put them in a second addon").on_hover_text("Held back; start a new project from them afterwards").clicked() {
                    choice = Some(OnConflict::SeparateAddon);
                }
                if ui.button("Leave them out").clicked() {
                    choice = Some(OnConflict::Skip);
                }
                if ui.button("Cancel the whole import").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            self.pending = None;
        } else if let Some(c) = choice {
            if let Some(plan) = self.pending.take() {
                let fresh = plan.fresh.len();
                let conflicts = plan.conflicts.len();
                let separate = clothing::apply_import(&mut self.project, plan, c);
                self.held.extend(separate);
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
                self.message = Some(Ok(format!("Added {fresh} drawable(s); {conflicts} that clashed: {}.", match c {
                    OnConflict::Continue => "numbered after the last one",
                    OnConflict::SeparateAddon => "held back for a second addon",
                    OnConflict::Skip => "left out",
                })));
            }
        }
    }

    /// Left: the drawables, by sex and component, with a search box; a mark on the ones with problems.
    fn list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Drawables").strong());
            ui.label(egui::RichText::new(self.project.drawables.len().to_string()).color(crate::theme::accent()));
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search...").desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            // fold every type: what is left is the outline (DLCs, sexes, types with their counts)
            if ui.small_button("Fold all").on_hover_text("Fold every type, so only the DLCs, sexes and types show; click one to open it").clicked() {
                for d in &self.project.drawables {
                    self.folded.insert(format!("d{}s{:?}k{:?}", d.dlc, d.sex, d.kind));
                }
            }
            if ui.small_button("Fold DLCs").on_hover_text("Fold every DLC").clicked() {
                for k in 0..self.project.dlcs.len() {
                    self.folded.insert(format!("d{k}"));
                }
            }
            if ui.small_button("Unfold all").clicked() {
                self.folded.clear();
            }
        });
        let find = self.search.trim().to_lowercase();
        // the worst problem of each drawable, for its mark
        let mut worst: Vec<Option<Sev>> = vec![None; self.project.drawables.len()];
        for x in self.issues().iter() {
            if let Some(w) = x.drawable.and_then(|i| worst.get_mut(i)) {
                *w = Some(w.map_or(x.sev, |s| s.min(x.sev)));
            }
        }
        let modifiers = ui.input(|i| i.modifiers);
        let keys: Vec<Key> = self.project.drawables.iter().map(key).collect();
        let mut clicked: Option<Key> = None;
        let mut archived_click: Option<usize> = None;
        let mut toggle: Option<String> = None;

        // ---- the rows, in order (cheap: nothing is laid out or drawn here)
        enum Row {
            Dlc(usize),
            Sex(usize, Sex),
            Kind(usize, Sex, Kind),
            Resource(String),
            Item(usize),
            RemovedHeader,
            Removed(usize),
            Empty,
        }
        let p = &self.project;
        let split = p.layout == clothing::Layout::ControlAndStreams;
        let searching = !find.is_empty();
        let folded = |k: &str| !searching && self.folded.contains(k);
        let mut per_dlc: std::collections::HashMap<usize, usize> = Default::default();
        let mut per_sex: std::collections::HashMap<(usize, Sex), usize> = Default::default();
        let mut per_group: std::collections::HashMap<(usize, Sex, Kind), usize> = Default::default();
        for d in &p.drawables {
            *per_dlc.entry(d.dlc).or_default() += 1;
            *per_sex.entry((d.dlc, d.sex)).or_default() += 1;
            *per_group.entry(d.group()).or_default() += 1;
        }
        let mut rows: Vec<Row> = Vec::with_capacity(p.drawables.len() + 64);
        let (mut last_dlc, mut last_sex, mut last_group, mut last_block) = (None, None, None, None);
        let (mut dlc_folded, mut sex_folded, mut group_folded) = (false, false, false);
        for (i, d) in p.drawables.iter().enumerate() {
            if searching {
                let name = format!("{}_{:03}", d.kind.name(), d.number);
                if !name.contains(&find) && !clothing::file_of(&d.ydd).to_lowercase().contains(&find) {
                    continue;
                }
            }
            if last_dlc != Some(d.dlc) {
                last_dlc = Some(d.dlc);
                (last_sex, last_group) = (None, None);
                dlc_folded = folded(&format!("d{}", d.dlc));
                rows.push(Row::Dlc(d.dlc));
            }
            if dlc_folded {
                continue;
            }
            if last_sex != Some((d.dlc, d.sex)) {
                last_sex = Some((d.dlc, d.sex));
                last_group = None;
                sex_folded = folded(&format!("d{}s{:?}", d.dlc, d.sex));
                rows.push(Row::Sex(d.dlc, d.sex));
            }
            if sex_folded {
                continue;
            }
            if last_group != Some(d.group()) {
                last_group = Some(d.group());
                last_block = None;
                group_folded = folded(&format!("d{}s{:?}k{:?}", d.dlc, d.sex, d.kind));
                rows.push(Row::Kind(d.dlc, d.sex, d.kind));
            }
            if group_folded {
                continue;
            }
            // which stream resource streams it: said at the start of a group and where a type passes the limit
            if split {
                let block = d.number as usize / p.stream_limit.max(1);
                if last_block != Some(block) {
                    last_block = Some(block);
                    rows.push(Row::Resource(build::stream_resource(p, d)));
                }
            }
            rows.push(Row::Item(i));
        }
        if !p.archived.is_empty() {
            rows.push(Row::RemovedHeader);
            rows.extend((0..p.archived.len()).map(Row::Removed));
        }
        if p.drawables.is_empty() {
            rows.push(Row::Empty);
        }
        let height = |r: &Row| match r {
            Row::Dlc(_) => 32.0,
            Row::Sex(..) | Row::Kind(..) => 22.0,
            Row::Resource(_) => 18.0,
            Row::Item(_) => 26.0,
            Row::RemovedHeader => 30.0,
            Row::Removed(_) => 22.0,
            Row::Empty => 60.0,
        };
        let total: f32 = rows.iter().map(height).sum();

        // ---- only the rows in view are laid out and drawn (a big pack has thousands)
        egui::ScrollArea::vertical().id_salt("cl_list").auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            ui.set_height(total);
            let origin = ui.min_rect().min;
            let w = ui.available_width();
            let mut y = 0.0;
            for (n, r) in rows.iter().enumerate() {
                let h = height(r);
                let visible = y + h >= viewport.min.y && y <= viewport.max.y;
                let rect = egui::Rect::from_min_size(origin + egui::vec2(0.0, y), egui::vec2(w, h));
                y += h;
                if !visible {
                    continue;
                }
                let id = ui.scope_id().with(("cl_row", n));
                // a heading row: an arrow (drawn, the font has none) and the label, indented by its level; a click anywhere on
                // the row folds or unfolds what is under it
                let mut heading = |ui: &mut egui::Ui, rect: egui::Rect, key: String, level: f32, label: String, style: fn(egui::RichText) -> egui::RichText, tip: String| {
                    let is_folded = folded(&key);
                    let resp = ui.interact(rect, id, egui::Sense::click());
                    let tip = if searching { "Searching shows everything; clear the search to fold".to_string() } else if tip.is_empty() { format!("Click to {}", if is_folded { "unfold" } else { "fold" }) } else { format!("{tip}\nClick to {}", if is_folded { "unfold" } else { "fold" }) };
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
                    let painter = ui.painter();
                    if resp.hovered() {
                        painter.rect_filled(rect, 4.0, crate::theme::surface_hi());
                    }
                    let indent = 2.0 + level * 14.0;
                    let c = egui::pos2(rect.left() + indent + 6.0, rect.center().y);
                    let arrow = if is_folded {
                        vec![c + egui::vec2(-3.0, -5.0), c + egui::vec2(-3.0, 5.0), c + egui::vec2(4.0, 0.0)]
                    } else {
                        vec![c + egui::vec2(-5.0, -3.0), c + egui::vec2(5.0, -3.0), c + egui::vec2(0.0, 4.0)]
                    };
                    painter.add(egui::Shape::convex_polygon(arrow, if resp.hovered() { crate::theme::text() } else { crate::theme::dim() }, egui::Stroke::NONE));
                    let galley = egui::WidgetText::from(style(egui::RichText::new(label))).into_galley(ui, Some(egui::TextWrapMode::Truncate), rect.width() - indent - 18.0, egui::TextStyle::Body);
                    ui.painter().galley(egui::pos2(rect.left() + indent + 16.0, rect.center().y - galley.size().y / 2.0), galley, crate::theme::text());
                    if resp.clicked() && !searching {
                        toggle = Some(key);
                    }
                };
                match r {
                    Row::Dlc(k) => {
                        let base = p.dlcs.get(*k).is_some_and(|x| x.base);
                        let label = format!("{}{}  ({})", p.dlc_label(*k), if base { "  Rockstar's" } else { "" }, per_dlc[k]);
                        let lower = egui::Rect::from_min_max(rect.min + egui::vec2(0.0, 6.0), rect.max);
                        heading(ui, lower, format!("d{k}"), 0.0, label, |t| t.strong().size(15.0).color(crate::theme::text()), format!("DLC {} / {}: one YMT and shop meta per sex", p.dlc_name(*k, Sex::Male), p.dlc_name(*k, Sex::Female)));
                    }
                    Row::Sex(k, sex) => heading(ui, rect, format!("d{k}s{sex:?}"), 1.0, format!("{}  ({})", sex.label().to_uppercase(), per_sex[&(*k, *sex)]), |t| t.color(crate::theme::accent()).strong(), format!("{}_{}.ymt", sex.ped(), p.dlc_name(*k, *sex))),
                    Row::Kind(k, sex, kind) => heading(ui, rect, format!("d{k}s{sex:?}k{kind:?}"), 2.0, format!("{}  ({})", kind.name(), per_group[&(*k, *sex, *kind)]), |t| t.color(crate::theme::dim()), String::new()),
                    Row::Resource(name) => {
                        ui.painter().text(egui::pos2(rect.left() + 48.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("in {name}"), egui::FontId::proportional(11.0), crate::theme::info());
                        ui.interact(rect, id, egui::Sense::hover()).on_hover_text(format!("At most {} drawables of a type and sex per stream resource; the rest go to the next (_02, _03, ...)", p.stream_limit));
                    }
                    Row::Item(i) => {
                        let d = &p.drawables[*i];
                        let resp = ui.interact(rect, id, egui::Sense::click());
                        let on = self.selected.contains(&key(d));
                        let pt = ui.painter();
                        pt.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 5.0, if on { ui.visuals().selection.bg_fill } else if resp.hovered() { crate::theme::surface_hi() } else { crate::theme::surface() });
                        // a coloured edge for the sex, a dot for the state, like other clothing tools
                        pt.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 1.0), egui::vec2(3.0, rect.height() - 2.0)), 2.0, if d.sex == Sex::Female { Color32::from_rgb(230, 90, 150) } else { Color32::from_rgb(80, 150, 240) });
                        let dot = match worst[*i] {
                            Some(Sev::Error) => crate::theme::err(),
                            Some(Sev::Warning) => crate::theme::warn(),
                            _ => crate::theme::ok(),
                        };
                        pt.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, dot);
                        let renumbered = if d.source_number != d.number { format!("  (was {:03})", d.source_number) } else { String::new() };
                        pt.text(egui::pos2(rect.left() + 26.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("{}_{:03}{renumbered}", d.kind.name(), d.number), egui::FontId::proportional(14.0), crate::theme::text());
                        // what is set on it: textures, cloth, and marks for its settings and texture changes
                        let mut marks = vec![format!("{} tex", d.textures.len())];
                        if d.cloth.is_some() {
                            marks.push("cloth".into());
                        }
                        if d.meta.heels().is_some() || d.meta.hair_scale().is_some() || d.meta.hides_hair {
                            marks.push("set".into());
                        }
                        if d.textures.iter().any(|t| t.edit.is_some()) || !d.model_edits.is_empty() {
                            marks.push("edited".into());
                        }
                        pt.text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, marks.join(", "), egui::FontId::proportional(12.0), crate::theme::dim());
                        if resp.clicked() {
                            clicked = Some(key(d));
                        }
                    }
                    Row::RemovedHeader => {
                        ui.painter().text(egui::pos2(rect.left() + 4.0, rect.bottom() - 10.0), egui::Align2::LEFT_CENTER, format!("REMOVED ({})", p.archived.len()), egui::FontId::proportional(14.0), crate::theme::warn());
                        ui.interact(rect, id, egui::Sense::hover()).on_hover_text("Taken out of the pack, kept with their files. Click one to restore it, move it to another pack or delete it.");
                    }
                    Row::Removed(j) => {
                        let d = &p.archived[*j];
                        let resp = ui.interact(rect, id, egui::Sense::click());
                        let on = self.archived_sel == Some(*j);
                        if on || resp.hovered() {
                            ui.painter().rect_filled(rect, 4.0, if on { ui.visuals().selection.bg_fill } else { crate::theme::surface_hi() });
                        }
                        ui.painter().text(egui::pos2(rect.left() + 10.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("{} {}_{:03}", d.sex.label(), d.kind.name(), d.number), egui::FontId::proportional(13.0), crate::theme::dim());
                        if resp.clicked() {
                            archived_click = Some(*j);
                        }
                    }
                    Row::Empty => {
                        let galley = egui::WidgetText::from(egui::RichText::new("Drop .ydd models and their textures (.ytd or images) here, or use + FEMALE / + MALE below.").color(crate::theme::dim())).into_galley(ui, Some(egui::TextWrapMode::Wrap), rect.width() - 8.0, egui::TextStyle::Body);
                        ui.painter().galley(rect.min + egui::vec2(4.0, 20.0), galley, crate::theme::dim());
                    }
                }
            }
        });
        if let Some(k) = toggle {
            if !self.folded.remove(&k) {
                self.folded.insert(k);
            }
        }
        if let Some(j) = archived_click {
            self.archived_sel = Some(j);
            self.selected.clear();
            self.preview_tex = 0;
            self.confirm_delete = false;
        }
        if let Some(k) = clicked {
            self.archived_sel = None;
            if modifiers.shift {
                let a = self.anchor.and_then(|a| keys.iter().position(|x| *x == a));
                let b = keys.iter().position(|x| *x == k);
                if let (Some(a), Some(b)) = (a, b) {
                    if !modifiers.command {
                        self.selected.clear();
                    }
                    self.selected.extend(keys[a.min(b)..=a.max(b)].iter().copied());
                }
            } else if modifiers.command {
                if !self.selected.remove(&k) {
                    self.selected.insert(k);
                }
                self.anchor = Some(k);
            } else {
                self.selected.clear();
                self.selected.insert(k);
                self.anchor = Some(k);
                self.preview_tex = 0;
            }
        }
    }

    fn move_selected(&mut self, by: i32) {
        // one at a time, the ones in front first, so a block moves together and stops at the end
        let mut order: Vec<Key> = self.selected.iter().copied().collect();
        if by > 0 {
            order.reverse();
        }
        let mut now = BTreeSet::new();
        for k in order {
            let Some(i) = self.project.drawables.iter().position(|d| key(d) == k) else { continue };
            // the drawable is found again by its id, because swapping changes the numbers
            let id = self.project.drawables[i].id;
            self.project.move_drawable(i, by);
            if let Some(d) = self.project.find(id) {
                now.insert(key(d));
            }
        }
        self.selected = now;
        self.dirty = true;
            self.rev += 1;
        self.plan = None;
    }

    /// Remove: the selected drawables go to the archive, with their files.
    fn archive_selected(&mut self) {
        let sel = self.selected.clone();
        let mut i = self.project.drawables.len();
        while i > 0 {
            i -= 1;
            if sel.contains(&key(&self.project.drawables[i])) {
                self.project.archive(i);
            }
        }
        self.selected.clear();
        self.changed(format!("{} drawable(s) moved to Removed (nothing deleted)", sel.len()));
    }

    fn changed(&mut self, what: String) {
        self.dirty = true;
            self.rev += 1;
        self.plan = None;
        self.message = Some(Ok(what));
    }

    /// Copy to / Move to another open pack: a menu of the other packs. True when something was sent.
    fn send_menu(&mut self, ui: &mut egui::Ui, d: &Drawable, copy: bool) -> bool {
        let mut sent = false;
        ui.add_enabled_ui(!self.others.is_empty(), |ui| {
            ui.menu_button(if copy { "Copy to pack" } else { "Move to pack" }, |ui| {
                for (k, name) in self.others.clone() {
                    if ui.button(&name).clicked() {
                        self.outbox.push((d.clone(), k));
                        sent = true;
                        ui.close();
                    }
                }
            })
            .response
            .on_disabled_hover_text("Open or start another pack (the tabs above) first");
        });
        sent
    }

    /// The card of a removed drawable: what it was, its files, and what to do with it.
    fn archived_card(&mut self, ui: &mut egui::Ui, j: usize) {
        let Some(d) = self.project.archived.get(j).cloned() else {
            self.archived_sel = None;
            return;
        };
        ui.label(egui::RichText::new(format!("Removed: {} {}_{:03}", d.sex.label(), d.kind.name(), d.number)).size(16.0).strong().color(crate::theme::warn()));
        ui.weak("Kept with its files, which were never touched. Nothing is deleted unless you press Delete permanently.");
        ui.add_space(6.0);
        ui.label(format!("Model: {}", clothing::file_of(&d.ydd)));
        if let Some(c) = &d.cloth {
            ui.label(format!("Cloth: {}", clothing::file_of(c)));
        }
        for t in &d.textures {
            ui.label(format!("Texture {}: {}", t.letter, clothing::file_of(&t.path)));
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Restore").on_hover_text("Back into this pack, after the last drawable of its kind").clicked() {
                if let Some(n) = self.project.restore(j) {
                    self.archived_sel = None;
                    self.changed(format!("Restored as {}_{n:03}", d.kind.name()));
                }
            }
            if self.send_menu(ui, &d, false) {
                self.project.archived.remove(j);
                self.archived_sel = None;
                self.changed("Moved to the other pack".into());
            }
            if self.confirm_delete {
                if ui.button(egui::RichText::new("Really delete it from the project").color(crate::theme::err())).on_hover_text("Its files on disk stay where they are").clicked() {
                    self.project.archived.remove(j);
                    self.archived_sel = None;
                    self.confirm_delete = false;
                    self.changed("Deleted from the project (its files on disk are untouched)".into());
                }
                if ui.button("Keep it").clicked() {
                    self.confirm_delete = false;
                }
            } else if ui.button("Delete permanently...").clicked() {
                self.confirm_delete = true;
            }
        });
    }

    /// Move to slot: the drawable's DLC, sex, type and number, with every file following (names change at build).
    fn slot_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        let d = &self.project.drawables[i];
        let k = key(d);
        if self.slot.as_ref().map(|s| s.0) != Some(k) {
            self.slot = Some((k, k));
        }
        let Some((_, (mut dlc, mut sex, mut kind, mut number))) = self.slot else { return };
        let prop = d.kind.is_prop();
        let dlcs: Vec<String> = (0..self.project.dlcs.len()).map(|n| self.project.dlc_label(n)).collect();
        ui.horizontal_wrapped(|ui| {
            ui.label("Move to slot");
            if dlcs.len() > 1 {
                egui::ComboBox::from_id_salt("cl_slot_dlc").selected_text(dlcs.get(dlc).cloned().unwrap_or_default()).width(140.0).show_ui(ui, |ui| {
                    for (n, name) in dlcs.iter().enumerate() {
                        ui.selectable_value(&mut dlc, n, name);
                    }
                });
            }
            egui::ComboBox::from_id_salt("cl_slot_sex").selected_text(sex.label()).width(80.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut sex, Sex::Female, "female");
                ui.selectable_value(&mut sex, Sex::Male, "male");
            });
            egui::ComboBox::from_id_salt("cl_slot_kind").selected_text(kind.name()).width(90.0).show_ui(ui, |ui| {
                for k in Kind::all().into_iter().filter(|k| k.is_prop() == prop) {
                    ui.selectable_value(&mut kind, k, k.name());
                }
            });
            ui.add(egui::DragValue::new(&mut number).range(0..=999).prefix("#"));
            let to = (dlc, sex, kind, number);
            let taken = self.project.drawables.iter().any(|x| x.slot() == to && key(x) != k);
            let tip = if taken { "That slot is taken: the two swap (or the other one moves to its group's next free number)" } else { "Its model, textures and cloth go with it; their file names follow when the pack is built" };
            if ui.add_enabled(to != k, egui::Button::new(if taken { "Swap" } else { "Move" })).on_hover_text(tip).clicked() {
                match self.project.move_to(i, to) {
                    Ok(()) => {
                        self.selected.clear();
                        self.selected.insert(to);
                        self.changed(format!("Moved to {} {}_{number:03}", sex.label(), kind.name()));
                    }
                    Err(e) => self.message = Some(Err(e)),
                }
            }
        });
        self.slot = self.slot.map(|s| (s.0, (dlc, sex, kind, number)));
    }

    /// the one selected drawable's index, when exactly one is selected
    fn single(&self) -> Option<usize> {
        let mut it = self.selected.iter();
        let k = it.next().filter(|_| it.next().is_none())?;
        self.project.drawables.iter().position(|d| key(d) == *k)
    }

    /// Right: the 3D preview of the selected drawable, with its texture choice and the body switch.
    fn preview_panel(&mut self, ui: &mut egui::Ui, render: Option<&eframe::egui_wgpu::RenderState>, game: Option<&Game>) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("3D preview").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.preview_body, "Body").on_hover_text(if game.is_some() { "Show it on the bare freemode body (from your game)" } else { "GTA V was not found: no body" });
                let name = super::cloth_preview::LODS.iter().find(|l| l.0 == self.preview_lod).map_or("High", |l| l.1);
                egui::ComboBox::from_id_salt("cl_preview_lod").selected_text(format!("LOD: {name}")).width(110.0).show_ui(ui, |ui| {
                    for (l, n) in super::cloth_preview::LODS {
                        ui.selectable_value(&mut self.preview_lod, l, n);
                    }
                }).response.on_hover_text("The level of detail shown: High is up close, the others what the game shows further away (the body follows)");
            });
        });
        let shown = match self.archived_sel {
            Some(j) => self.project.archived.get(j),
            None => self.single().map(|i| &self.project.drawables[i]),
        };
        let want = shown.map(|d| {
            super::cloth_preview::Want { ydd: d.ydd.clone(), texture: d.textures.get(self.preview_tex).or(d.textures.first()).map(|t| t.path.clone()), sex: d.sex, kind: d.kind, body: self.preview_body, lod: self.preview_lod }
        });
        self.preview.ui(ui, render, want, game);
    }

    /// Middle: the selected drawable, like other clothing tools' card: what it is, its files, its textures, its problems.
    fn texture_info(&mut self, p: &Path) -> Option<Vec<TexInfo>> {
        if p.as_os_str().is_empty() {
            return None;
        }
        self.tex_info.entry(p.to_path_buf()).or_insert_with(|| read_tex_info(p)).clone()
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        if let Some(j) = self.archived_sel {
            self.archived_card(ui, j);
            return;
        }
        let Some(i) = self.single() else {
            ui.add_space(20.0);
            ui.weak(if self.selected.is_empty() { "Click a drawable on the left to see its files and textures." } else { "Several drawables selected: Remove, Move up / down work on all of them." });
            return;
        };
        let issues = self.issues();
        // what the files hold (read once per file, kept)
        let (ydd, tex_paths) = (self.project.drawables[i].ydd.clone(), self.project.drawables[i].textures.iter().map(|t| t.path.clone()).collect::<Vec<_>>());
        let tex_infos: Vec<Option<TexInfo>> = tex_paths.iter().map(|p| self.texture_info(p).and_then(|v| v.into_iter().next())).collect();
        let tex_counts: Vec<usize> = tex_paths.iter().map(|p| self.texture_info(p).map_or(0, |v| v.len())).collect();
        let embedded = self.texture_info(&ydd).unwrap_or_default();

        // ---- where it goes
        {
            let p = &self.project;
            let d = &p.drawables[i];
            let dlc = p.dlc_name(d.dlc, d.sex);
            let in_group = p.drawables.iter().filter(|x| x.group() == d.group()).count();
            section(ui, &format!("{} {}_{:03}", d.sex.label(), d.kind.name(), d.number), "where it goes in the game and in the pack", |ui| {
                egui::Grid::new("cl_where").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new("DLC").color(crate::theme::dim()));
                    ui.label(format!("{dlc}{}  ({} of {in_group} {} {} drawables in it, at most {})", if p.dlcs.get(d.dlc).is_some_and(|x| x.base) { " (Rockstar's DLC, replaced)" } else { "" }, d.number + 1, d.sex.label(), d.kind.name(), p.limit));
                    ui.end_row();
                    ui.label(egui::RichText::new("YMT").color(crate::theme::dim()));
                    ui.label(format!("{}_{dlc}.ymt, {} {}", d.sex.ped(), if d.kind.is_prop() { "prop" } else { "component" }, d.number));
                    ui.end_row();
                    ui.label(egui::RichText::new("Streamed from").color(crate::theme::dim()));
                    ui.label(if p.layout == clothing::Layout::ControlAndStreams { format!("{}  (at most {} of a type each)", build::stream_resource(p, d), p.stream_limit) } else { format!("{} (one resource)", p.name) });
                    ui.end_row();
                    if d.source_number != d.number {
                        ui.label(egui::RichText::new("Its file was").color(crate::theme::dim()));
                        ui.label(format!("number {:03} (renumbered)", d.source_number));
                        ui.end_row();
                    }
                });
            });
        }

        // ---- slot
        section(ui, "Slot", "move it to another number, type, sex or DLC, or to another open pack", |ui| {
            self.slot_editor(ui, i);
        });
        let Some(i) = self.single() else { return };
        let this = self.project.drawables[i].clone();
        let mut moved_away = false;
        ui.horizontal_wrapped(|ui| {
            self.send_menu(ui, &this, true);
            if self.send_menu(ui, &this, false) {
                moved_away = true;
            }
        });
        if moved_away {
            self.project.drawables.remove(i);
            self.selected.clear();
            self.changed("Moved to the other pack".into());
            return;
        }
        let Some(i) = self.single() else { return };
        let mut changed = false;
        let mut archive_it = false;
        let mut remove = None;
        let creature = self.project.creature_name(self.project.drawables[i].dlc);
        let base_dlc = self.project.dlcs.get(self.project.drawables[i].dlc).is_some_and(|x| x.base);
        let d = &mut self.project.drawables[i];

        // ---- files
        let file_row = |ui: &mut egui::Ui, label: &str, path: Option<&PathBuf>| {
            ui.label(egui::RichText::new(label).color(crate::theme::dim()));
            match path {
                Some(p) if p.as_os_str().is_empty() => ui.weak("not in the pack (the game's own, or missing)"),
                // a built pack's caret names are long: the part after the caret, the whole path on hover
                Some(p) => ui.label(egui::RichText::new(short(p)).color(if p.is_file() { crate::theme::text() } else { crate::theme::err() })).on_hover_text(p.display().to_string()),
                None => ui.weak("none"),
            };
        };
        section(ui, "Files", "the model, its alternates and cloth physics", |ui| {
            egui::Grid::new("cl_files").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                file_row(ui, "Model (.ydd)", Some(&d.ydd));
                ui.horizontal(|ui| {
                    if ui.button("Replace...").clicked() {
                        if let Some(f) = rfd::FileDialog::new().add_filter("Model", &["ydd"]).pick_file() {
                            d.ydd = f;
                            changed = true;
                        }
                    }
                    if ui.button("Remove").on_hover_text("Move it to Removed with its textures and cloth (nothing is deleted)").clicked() {
                        archive_it = true;
                    }
                });
                ui.end_row();
                for (n, a) in &d.alternates {
                    file_row(ui, &format!("Alternate model _{n}"), Some(a));
                    ui.weak("goes with the drawable");
                    ui.end_row();
                }
                file_row(ui, "Cloth physics (.yld)", d.cloth.as_ref());
                ui.horizontal(|ui| {
                    if ui.button(if d.cloth.is_some() { "Replace..." } else { "Add..." }).clicked() {
                        if let Some(f) = rfd::FileDialog::new().add_filter("Cloth", &["yld"]).pick_file() {
                            d.cloth = Some(f);
                            changed = true;
                        }
                    }
                    if d.cloth.is_some() && ui.button("Remove").clicked() {
                        d.cloth = None;
                        changed = true;
                    }
                });
                ui.end_row();
            });
        });

        // ---- textures
        let note = "the round button picks the one in the 3D preview; size and format change only the built pack, your files stay as they are";
        section(ui, &format!("Textures ({})", d.textures.len()), note, |ui| {
            if d.textures.len() > 1 {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("All textures:").color(crate::theme::dim()));
                    let mut all = d.textures.iter().find_map(|t| t.edit.clone()).unwrap_or_default();
                    // the smallest of them, so no size choice offers to enlarge one
                    let smallest = tex_infos.iter().flatten().min_by_key(|t| t.width.max(t.height)).cloned();
                    if edit_controls(ui, "cl_tex_all", &mut all, smallest.as_ref()) {
                        for t in &mut d.textures {
                            t.edit = (!all.is_empty()).then(|| all.clone());
                        }
                        changed = true;
                    }
                });
                ui.separator();
            }
            for (n, t) in d.textures.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.preview_tex, n, egui::RichText::new(t.letter.to_string()).monospace().strong()).on_hover_text("Show this one in the 3D preview");
                    if t.path.as_os_str().is_empty() {
                        ui.weak("not in the pack");
                        return;
                    }
                    ui.label(egui::RichText::new(short(&t.path)).color(if t.path.is_file() { crate::theme::text() } else { crate::theme::err() })).on_hover_text(t.path.display().to_string());
                    let info = tex_infos.get(n).cloned().flatten();
                    match &info {
                        Some(x) => {
                            let more = if tex_counts[n] > 1 { format!(" (+{} more)", tex_counts[n] - 1) } else { String::new() };
                            // for a .ytd with several textures the memory is all of them; the file is the one .ytd
                            let memory: usize = self.tex_info.get(&t.path).cloned().flatten().map_or(x.bytes, |v| v.iter().map(|i| i.bytes).sum());
                            ui.label(egui::RichText::new(format!("{}x{} {}  {} in memory, {} file{more}", x.width, x.height, x.format, mb(memory), mb(x.file as usize))).small().color(crate::theme::dim())).on_hover_text(MEMORY_TIP);
                        }
                        None if t.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ytd")) => {
                            ui.label(egui::RichText::new("cannot be read").small().color(crate::theme::warn()));
                        }
                        None => {
                            ui.label(egui::RichText::new("image: made into a .ytd at build").small().color(crate::theme::dim()));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("x").on_hover_text("Remove this texture").clicked() {
                            remove = Some(n);
                        }
                        if ui.small_button("Replace...").clicked() {
                            if let Some(f) = rfd::FileDialog::new().add_filter("Texture", &["ytd", "png", "jpg", "jpeg", "dds", "tga", "bmp"]).pick_file() {
                                t.path = f;
                                changed = true;
                            }
                        }
                        if t.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ytd")) {
                            let mut e = t.edit.clone().unwrap_or_default();
                            if edit_controls(ui, &format!("cl_tex_{n}"), &mut e, info.as_ref()) {
                                t.edit = (!e.is_empty()).then_some(e);
                                changed = true;
                            }
                            if let Some(e) = &t.edit {
                                ui.label(egui::RichText::new(edit_note(e)).small().color(crate::theme::info()));
                            }
                        }
                    });
                });
            }
            if d.textures.is_empty() {
                ui.weak("No texture yet: every drawable needs at least one (jbib_diff_000_a_uni.ytd, or an image).");
            }
            ui.add_space(4.0);
            if ui.button("ADD TEXTURES").clicked() {
                if let Some(fs) = rfd::FileDialog::new().add_filter("Texture or image", &["ytd", "png", "jpg", "jpeg", "dds", "tga", "bmp"]).pick_files() {
                    for f in fs {
                        let letter = (b'a' + d.textures.len().min(25) as u8) as char;
                        d.textures.push(Texture { path: f, letter, race: String::new(), tex_id: None, edit: None });
                    }
                    changed = true;
                }
            }
        });

        // ---- textures inside the model
        let model_note = match embedded.first() {
            // the textures are compressed together with the geometry: one file size for the model
            Some(t) => format!("normal and specular maps stored in the .ydd; changes go to the built model and its alternates. {} in memory together; the model file is {} on disk (textures and geometry together)", mb(embedded.iter().map(|x| x.bytes).sum()), mb(t.file as usize)),
            None => "normal and specular maps stored in the .ydd; changes go to the built model and its alternates".to_string(),
        };
        section(ui, &format!("Textures inside the model ({})", embedded.len()), &model_note, |ui| {
            if embedded.is_empty() {
                ui.weak(if d.ydd.as_os_str().is_empty() { "The model is the game's own." } else { "None, or the model cannot be read." });
                return;
            }
            if embedded.len() > 1 {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("All of them:").color(crate::theme::dim()));
                    let mut all = d.model_edits.first().map(|x| x.1.clone()).unwrap_or_default();
                    let smallest = embedded.iter().min_by_key(|t| t.width.max(t.height)).cloned();
                    if edit_controls(ui, "cl_emb_all", &mut all, smallest.as_ref()) {
                        d.model_edits = if all.is_empty() { vec![] } else { embedded.iter().map(|t| (t.name.clone(), all.clone())).collect() };
                        changed = true;
                    }
                });
                ui.separator();
            }
            egui::Grid::new("cl_embedded").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                for (k, t) in embedded.iter().enumerate() {
                    ui.label(&t.name);
                    ui.label(egui::RichText::new(format!("{}x{} {}  {} in memory", t.width, t.height, t.format, mb(t.bytes))).small().color(crate::theme::dim())).on_hover_text("The pixel data with every mip level, what the game holds once it is streamed in. Inside a model the textures are compressed together with the geometry, so there is no file size per texture: the model's is in the line above.");
                    let at = d.model_edits.iter().position(|(n, _)| n.eq_ignore_ascii_case(&t.name));
                    let mut e = at.map(|j| d.model_edits[j].1.clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        if edit_controls(ui, &format!("cl_emb_{k}"), &mut e, Some(t)) {
                            match (at, e.is_empty()) {
                                (Some(j), true) => {
                                    d.model_edits.remove(j);
                                }
                                (Some(j), false) => d.model_edits[j].1 = e.clone(),
                                (None, false) => d.model_edits.push((t.name.clone(), e.clone())),
                                (None, true) => {}
                            }
                            changed = true;
                        }
                    });
                    ui.label(egui::RichText::new(if e.is_empty() { String::new() } else { edit_note(&e) }).small().color(crate::theme::info()));
                    ui.end_row();
                }
            });
        });

        // ---- game settings
        section(ui, "Game settings (YMT)", "what the game does with it; written into the DLC's YMT", |ui| {
            let m = &mut d.meta;
            match d.kind {
                Kind::Component(6) => {
                    ui.horizontal(|ui| {
                        let mut on = m.expression_mods[4] != 0.0;
                        if ui.checkbox(&mut on, "High heels: lift the ped by").on_hover_text("The ped stands higher while these are worn (expression value 4). 1.0 is a common heel.").changed() {
                            m.expression_mods[4] = if on { 1.0 } else { 0.0 };
                            changed = true;
                        }
                        if on {
                            changed |= ui.add(egui::DragValue::new(&mut m.expression_mods[4]).range(0.01..=3.0).speed(0.01)).changed();
                        }
                    });
                }
                Kind::Component(1) => {
                    changed |= ui.checkbox(&mut m.hides_hair, "Hides the hair (every hair shows its alternate under this mask)").on_hover_text("The build writes the hair switches into pedalternatevariations").changed();
                }
                Kind::Prop(0) => {
                    ui.horizontal(|ui| {
                        let mut on = m.expression_mods[0] != 0.0;
                        if ui.checkbox(&mut on, "Squashes the hair under it by").on_hover_text("How far the hair is pressed down under this hat (expression value 0); 0.5 is common").changed() {
                            m.expression_mods[0] = if on { -0.5 } else { 0.0 };
                            changed = true;
                        }
                        if on {
                            let mut amount = -m.expression_mods[0];
                            if ui.add(egui::DragValue::new(&mut amount).range(0.01..=1.0).speed(0.01)).changed() {
                                m.expression_mods[0] = -amount;
                                changed = true;
                            }
                        }
                    });
                }
                _ => {}
            }
            if !d.alternates.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("In first person show");
                    let label = m.first_person.map_or("the model itself".to_string(), |n| format!("alternate _{n}"));
                    egui::ComboBox::from_id_salt("cl_fp").selected_text(label).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut m.first_person, None, "the model itself").changed();
                        for (n, _) in &d.alternates {
                            changed |= ui.selectable_value(&mut m.first_person, Some(*n), format!("alternate _{n}")).changed();
                        }
                    });
                });
            }
            if d.kind.is_prop() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Render flags:");
                    for (name, bit) in clothing::ymt::render_flags() {
                        let mut on = m.render_flags & bit != 0;
                        if ui.checkbox(&mut on, name.trim_start_matches("PRF_").to_lowercase()).changed() {
                            m.render_flags = if on { m.render_flags | bit } else { m.render_flags & !bit };
                            changed = true;
                        }
                    }
                });
            } else {
                let set = clothing::COMP_FLAGS.iter().filter(|(b, _)| m.flags & b != 0).count();
                egui::CollapsingHeader::new(format!("Flags ({set} set)")).id_salt("cl_flags").show(ui, |ui| {
                    egui::Grid::new("cl_flag_grid").num_columns(3).show(ui, |ui| {
                        for (k, (bit, name)) in clothing::COMP_FLAGS.iter().enumerate() {
                            let mut on = m.flags & bit != 0;
                            if ui.checkbox(&mut on, *name).changed() {
                                m.flags = if on { m.flags | bit } else { m.flags & !bit };
                                changed = true;
                            }
                            if k % 3 == 2 {
                                ui.end_row();
                            }
                        }
                    });
                });
            }
            egui::CollapsingHeader::new("Advanced").id_salt("cl_adv").show(ui, |ui| {
                egui::Grid::new("cl_adv_grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                    let mut audio = m.audio.clone().unwrap_or_else(|| if d.kind.is_prop() { String::new() } else { "none".into() });
                    ui.label("Audio");
                    if ui.add(egui::TextEdit::singleline(&mut audio).desired_width(160.0)).on_hover_text("The sound it makes (pedXml_audioID / audioId): a name or hash_XXXXXXXX").changed() {
                        let usual = if d.kind.is_prop() { "" } else { "none" };
                        m.audio = (audio != usual).then_some(audio);
                        changed = true;
                    }
                    ui.end_row();
                    if !d.kind.is_prop() {
                        let usual = if d.race == "r" { 17 } else { 1 };
                        let mut mask = m.prop_mask.unwrap_or(usual);
                        ui.label("propMask");
                        if ui.add(egui::DragValue::new(&mut mask).range(0..=255)).on_hover_text(format!("Usually {usual} for this drawable ({}); packs also use 5, 11, 65", if d.race == "r" { "_r" } else { "_u" })).changed() {
                            m.prop_mask = (mask != usual).then_some(mask);
                            changed = true;
                        }
                        ui.end_row();
                    } else {
                        ui.label("Stickyness");
                        changed |= ui.add(egui::DragValue::new(&mut m.stickyness).range(0..=255)).changed();
                        ui.end_row();
                    }
                    ui.label(if d.kind.is_prop() { "propFlags" } else { "flags (number)" });
                    changed |= ui.add(egui::DragValue::new(&mut m.flags)).changed();
                    ui.end_row();
                    ui.label(if d.kind.is_prop() { "flags" } else { "pedXml_flags" });
                    changed |= ui.add(egui::DragValue::new(&mut m.flags2)).changed();
                    ui.end_row();
                    ui.label("Expression values");
                    ui.horizontal(|ui| {
                        for k in 0..5 {
                            changed |= ui.add(egui::DragValue::new(&mut m.expression_mods[k]).speed(0.01)).changed();
                        }
                    });
                    ui.end_row();
                });
            });
        });

        // ---- creature metadata
        section(ui, "Creature metadata", "the expression tracks that make heels lift the ped and hats press the hair", |ui| {
            let file = format!("{}.ymt", creature.to_lowercase());
            match d.kind {
                Kind::Component(6) | Kind::Prop(0) => {
                    let (what, value, part) = if d.kind == Kind::Prop(0) { ("hat", -d.meta.expression_mods[0], "hair squash") } else { ("feet", d.meta.expression_mods[4], "heel height") };
                    if base_dlc && self.project.dlcs.get(d.dlc).is_some_and(|x| x.creature.is_none()) {
                        ui.label(egui::RichText::new(format!("This is Rockstar's DLC and its creature metadata file was not in the pack: the game's own stays, with its own {what} entries.")).color(crate::theme::dim()));
                    } else {
                        ui.label(format!("Listed in {file} as {what} {} (every {what} of the DLC is, for both sexes).", d.number));
                        ui.label(egui::RichText::new(if value != 0.0 { format!("Its {part} above ({value:.2}) is what the track uses.") } else { format!("Its {part} is 0: the track is there but does nothing.") }).color(crate::theme::dim()));
                    }
                }
                _ => {
                    ui.label(egui::RichText::new(format!("Not used for {}: only feet (high heels) and hats (p_head, the hair under them) have creature metadata.", d.kind.name())).color(crate::theme::dim()));
                }
            }
        });

        if let Some(n) = remove {
            d.textures.remove(n);
            self.preview_tex = self.preview_tex.min(d.textures.len().saturating_sub(1));
            changed = true;
        }
        let mine: Vec<&clothing::Issue> = issues.iter().filter(|x| x.drawable == Some(i)).collect();
        if !mine.is_empty() {
            section(ui, "Problems", "", |ui| {
                for x in mine {
                    ui.label(egui::RichText::new(&x.message).color(sev_colour(x.sev)));
                    ui.weak(format!("How to fix: {}", x.fix));
                }
            });
        }
        if changed {
            self.dirty = true;
            self.rev += 1;
            self.plan = None;
        }
        if archive_it {
            self.archive_selected();
        }
    }

    /// Every check, with how to fix it; a click on a drawable selects it.
    fn checks_window(&mut self, ctx: &egui::Context) {
        if !self.checks_open {
            return;
        }
        let issues = self.issues();
        let mut select: Option<usize> = None;
        let mut open = true;
        egui::Window::new("Checks").open(&mut open).default_size([640.0, 380.0]).show(ctx, |ui| {
            if issues.is_empty() {
                ui.label(egui::RichText::new("No problems.").color(crate::theme::ok()));
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for i in issues.iter() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(match i.sev {
                            Sev::Error => "ERROR",
                            Sev::Warning => "WARN ",
                            Sev::Info => "NOTE ",
                        }).monospace().color(sev_colour(i.sev)));
                        if let Some(d) = i.drawable {
                            if ui.link(self.project.drawables.get(d).map(|x| x.label()).unwrap_or_default()).clicked() {
                                select = Some(d);
                            }
                        }
                        ui.label(&i.message);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(52.0);
                        ui.weak(format!("How to fix: {}", i.fix));
                    });
                }
            });
        });
        self.checks_open = open;
        if let Some(d) = select.and_then(|d| self.project.drawables.get(d)) {
            self.selected.clear();
            self.selected.insert(key(d));
        }
    }

    /// The DLCs the build writes, each its own new DLC or one of Rockstar's (the game's YMT, the DLC's drawables after the game's).
    fn dlc_choices(&mut self, ui: &mut egui::Ui, game: Option<&Game>) {
        let p = &self.project;
        if p.dlcs.is_empty() {
            return;
        }
        ui.label(egui::RichText::new("DLCs this build writes (each a new DLC, or one of Rockstar's: the game's YMT with these drawables after the game's)").small().color(crate::theme::dim()));
        let mut make_new: Option<usize> = None;
        let mut make_rockstar: Option<(usize, game_dlc::GameDlc)> = None;
        let mut toggled = false;
        egui::Grid::new("cl_build_dlcs").num_columns(4).spacing([10.0, 4.0]).striped(true).show(ui, |ui| {
            for k in 0..self.project.dlcs.len() {
                let p = &self.project;
                let ours = |sex: Sex| p.drawables.iter().filter(|d| d.dlc == k && d.sex == sex && !d.ydd.as_os_str().is_empty()).count();
                let game_slots = p.drawables.iter().filter(|d| d.dlc == k && d.ydd.as_os_str().is_empty()).count();
                let base = p.dlcs[k].base;
                ui.label(format!("{}.", k + 1));
                let current = if base { format!("Rockstar: {}", p.dlc_label(k)) } else { format!("new: {}", p.dlc_label(k)) };
                let new_name = format!("{}_{}", p.name, k + 1);
                let used: Vec<String> = (0..p.dlcs.len()).filter(|o| *o != k).map(|o| p.dlc_name(o, Sex::Male).to_lowercase()).collect();
                let busy = self.convert_task.busy();
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt(("cl_dlc_kind", k)).selected_text(current).width(220.0).height(360.0).show_ui(ui, |ui| {
                        if ui.selectable_label(!base, format!("new DLC: {new_name}")).on_hover_text("A DLC of the pack's own: its own YMT and shop meta").clicked() && base {
                            make_new = Some(k);
                        }
                        match (&self.rockstar, game) {
                            (Some(list), _) => {
                                for g in &list.dlcs {
                                    let name = g.name(Sex::Male);
                                    let taken = used.contains(&name);
                                    let packs: Vec<String> = [Sex::Male, Sex::Female].iter().filter_map(|s| g.pack(*s).map(|x| format!("{} {x}", if *s == Sex::Male { "m" } else { "f" }))).collect();
                                    let mine = base && p.dlc_name(k, Sex::Male).eq_ignore_ascii_case(&name);
                                    let r = ui.add_enabled(!taken, egui::Button::selectable(mine, format!("Rockstar: {}", g.label))).on_hover_text(format!("The game's YMT{}{}; this DLC's drawables go after the game's{}", if packs.is_empty() { String::new() } else { format!(" from {}", packs.join(", ")) }, if g.versions.iter().any(|v| *v > 1) { " (the newest of the game's versions)" } else { "" }, if taken { ". Another DLC of the project is this one already." } else { "" }));
                                    if r.clicked() && !mine {
                                        make_rockstar = Some((k, g.clone()));
                                    }
                                }
                            }
                            (None, Some(_)) => {
                                if let Some(e) = &self.rockstar_error {
                                    ui.colored_label(crate::theme::err(), format!("the game's DLCs could not be listed: {e}"));
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.weak("listing the game's DLCs...");
                                    });
                                }
                            }
                            (None, None) => {
                                ui.weak("Set the game folder to pick one of Rockstar's DLCs");
                            }
                        }
                    });
                });
                let (m, f) = (ours(Sex::Male), ours(Sex::Female));
                ui.label(egui::RichText::new(if base { format!("{m} male, {f} female of the pack after the game's {game_slots}") } else { format!("{m} male, {f} female drawables") }).small());
                if base {
                    let mut on = !self.project.dlcs[k].left_out;
                    if ui.checkbox(&mut on, "Replace its YMT").on_hover_text("Off: this Rockstar DLC is left out of the build (no YMT, none of its files); the game keeps its own").changed() {
                        self.project.dlcs[k].left_out = !on;
                        toggled = true;
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
        if let Some(k) = make_new {
            let gone = game_dlc::make_new(&mut self.project, k);
            self.message = Some(Ok(format!("DLC {} is a new DLC of the pack again ({}): the game's {gone} slot(s) went, its drawables are numbered from 000", k + 1, self.project.dlc_label(k))));
            self.selected.clear();
            toggled = true;
        }
        if let (Some((k, g)), Some(game), Some(list)) = (make_rockstar, game, self.rockstar.clone()) {
            let game = game.clone();
            self.message = None;
            self.convert_task.run(move || {
                let files = game_dlc::game_files(&game, &g)?;
                let races = [game_dlc::Races::of(&list.index, &g, Sex::Male), game_dlc::Races::of(&list.index, &g, Sex::Female)];
                Ok(Convert { dlc: k, game: g, files, races })
            });
        }
        if self.convert_task.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("reading the Rockstar DLC from the game...");
            });
        }
        if toggled {
            self.dirty = true;
            self.rev += 1;
            self.plan = None;
        }
    }

    /// Where to write, what will be written, and the Build button.
    fn build_window(&mut self, ctx: &egui::Context, game: Option<&Game>) {
        if !self.build_open {
            return;
        }
        // the game's DLCs, listed once (the file index is cached; the first time it walks the archives)
        if let Some(r) = self.rockstar_task.poll(ctx) {
            match r {
                Ok(list) => self.rockstar = Some(list),
                Err(e) => self.rockstar_error = Some(e),
            }
        }
        if let (Some(g), None, false, None) = (game, &self.rockstar, self.rockstar_task.busy(), &self.rockstar_error) {
            let g = g.clone();
            self.rockstar_task.run(move || {
                let (dlcs, index) = game_dlc::from_game(&g, &rage_toolkit_core::job::Job::new())?;
                Ok(std::sync::Arc::new(Rockstar { dlcs, index }))
            });
        }
        if let Some(r) = self.convert_task.poll(ctx) {
            match r {
                Ok(c) => {
                    let read = |path: &str| c.files.get(path).cloned().ok_or_else(|| anyhow::anyhow!("{path} was not read"));
                    match game_dlc::make_rockstar(&mut self.project, c.dlc, &c.game, &read, [&c.races[0], &c.races[1]]) {
                        Ok(m) => {
                            let on = if m.moved_on > 0 { format!("; {} no longer fitted under the limit and moved on to the next DLC", m.moved_on) } else { String::new() };
                            self.message = Some(Ok(format!("DLC {} is now Rockstar's {} / {}: the game's {} drawable(s) first, the DLC's {} after them{on}", c.dlc + 1, c.game.name(Sex::Male), c.game.name(Sex::Female), m.slots, m.kept)));
                            self.selected.clear();
                            self.dirty = true;
                            self.rev += 1;
                            self.plan = None;
                        }
                        Err(e) => self.message = Some(Err(format!("{e:#}"))),
                    }
                }
                Err(e) => self.message = Some(Err(e)),
            }
        }
        let errors = self.issues().iter().filter(|i| i.sev == Sev::Error).count();
        let mut open = true;
        egui::Window::new("Build the FiveM resources").open(&mut open).default_size([720.0, 460.0]).show(ctx, |ui| {
            let split = self.project.layout == clothing::Layout::ControlAndStreams;
            ui.horizontal(|ui| {
                ui.label("Write into");
                let hint = if split { "a folder; the resources go in it (<name>_control, <name>_stream_m1, ...)" } else { "a folder; the pack goes in <folder>/<pack name>" };
                ui.add(egui::TextEdit::singleline(&mut self.out_dir).desired_width(380.0).hint_text(hint));
                if ui.button("Browse...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.out_dir = d.display().to_string();
                    }
                }
            });
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label("Layout");
                changed |= ui.radio_value(&mut self.project.layout, clothing::Layout::Single, "One resource").on_hover_text("Everything in one resource named after the project").changed();
                changed |= ui.radio_value(&mut self.project.layout, clothing::Layout::ControlAndStreams, "Control + stream resources").on_hover_text("<name>_control holds the manifest, YMTs, metas and creature metadata; <name>_stream_m1, _f1, ... (sex and DLC number) only stream the files, at most the limit of one type each (the rest in _m1_02, ...); Rockstar's DLCs in <name>_stream_base").changed();
            });
            if split {
                ui.horizontal(|ui| {
                    ui.label("Per stream resource, at most");
                    changed |= ui.add(egui::DragValue::new(&mut self.project.stream_limit).range(1..=1000).suffix(" drawables of a type")).changed();
                });
            } else {
                changed |= ui.checkbox(&mut self.project.sorted, "Sort the streamed files into folders by category").on_hover_text("On: stream/female/jbib/, stream/male/lowr/, stream/male/p_head/ ... Off: everything in stream/. FiveM streams both the same way; it is only how the pack is laid out.").changed();
            }
            if changed {
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
            }
            self.dlc_choices(ui, game);
            // textures made smaller in this build only (the sources stay as they are): off until ticked, not saved with the
            // project, so it never happens unasked
            ui.label(egui::RichText::new("Shrink textures (only this build; not saved with the project)").small().color(crate::theme::dim()));
            let mut shrink_changed = false;
            ui.horizontal(|ui| {
                let o = &mut self.project.optimize;
                let mut on = o.ytd_max.is_some();
                if ui.checkbox(&mut on, "Shrink textures (.ytd) to at most").on_hover_text("Textures with a longer side are resized (a full mip chain, the format kept); the others are copied as they are. The sources are never changed.").changed() {
                    o.ytd_max = on.then_some(1024);
                    shrink_changed = true;
                }
                if let Some(m) = o.ytd_max.as_mut() {
                    egui::ComboBox::from_id_salt("cl_ytd_max").selected_text(format!("{m} px")).width(90.0).show_ui(ui, |ui| {
                        for v in [256u32, 512, 1024, 2048] {
                            shrink_changed |= ui.selectable_value(m, v, format!("{v} px")).changed();
                        }
                    });
                }
            });
            ui.horizontal(|ui| {
                let o = &mut self.project.optimize;
                let mut on = o.ydd_max.is_some();
                if ui.checkbox(&mut on, "Shrink textures inside models to at most").on_hover_text("Textures embedded in the .ydd (often an ATI2 normal map). Only the formats named are touched.").changed() {
                    o.ydd_max = on.then_some(512);
                    if on && o.ydd_formats.is_empty() {
                        o.ydd_formats = "ati2".into();
                    }
                    shrink_changed = true;
                }
                if let Some(m) = o.ydd_max.as_mut() {
                    egui::ComboBox::from_id_salt("cl_ydd_max").selected_text(format!("{m} px")).width(90.0).show_ui(ui, |ui| {
                        for v in [128u32, 256, 512, 1024] {
                            shrink_changed |= ui.selectable_value(m, v, format!("{v} px")).changed();
                        }
                    });
                    ui.label("formats");
                    shrink_changed |= ui.add(egui::TextEdit::singleline(&mut o.ydd_formats).desired_width(90.0).hint_text("all")).on_hover_text("ati2, dxt5, ... (comma separated); empty for all").changed();
                }
            });
            if changed {
                self.dirty = true;
            self.rev += 1;
                self.plan = None;
                self.size_text = None;
            }
            if shrink_changed {
                // the plan says which files go through the optimizer; the project itself is unchanged
                self.plan = None;
                self.rev += 1;
                self.size_text = None;
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.size_task.busy() && !self.project.drawables.is_empty(), egui::Button::new("Estimate texture sizes")).on_hover_text("Reads every texture of the pack: their sizes, and what the settings above would save").clicked() {
                    let (p, job) = (self.project.clone(), rage_toolkit_core::job::Job::new());
                    self.size_job = job.clone();
                    self.size_task.run(move || {
                        let r = build::texture_sizes(&p, &p.optimize, &|done, all| {
                            job.set_total(all);
                            if done > job.progress().0 {
                                job.step();
                            }
                        });
                        Ok(build::format_sizes(&r, &p.optimize))
                    });
                }
                if self.size_task.busy() {
                    let (d, t) = self.size_job.progress();
                    ui.add(egui::ProgressBar::new(if t > 0 { d as f32 / t as f32 } else { 0.0 }).desired_width(200.0).text(format!("{d}/{t} files")));
                }
            });
            if let Some(r) = self.size_task.poll(ctx) {
                self.size_text = Some(r.unwrap_or_else(|e| e));
            }
            if let Some(t) = &self.size_text {
                egui::CollapsingHeader::new("Texture sizes").default_open(true).id_salt("cl_sizes").show(ui, |ui| {
                    ui.label(egui::RichText::new(t).monospace().small());
                });
            }
            ui.checkbox(&mut self.update_build, "Update an earlier build there").on_hover_text("Only files that changed are written (optimized files again when the texture settings changed); files the build no longer has are listed, not deleted. Off: nothing that exists is overwritten.");
            if errors > 0 {
                ui.label(egui::RichText::new(format!("{errors} error(s) to fix first (the checks at the bottom of the page)")).color(crate::theme::err()));
            }
            let ok = !self.out_dir.trim().is_empty() && errors == 0 && !self.project.drawables.is_empty() && !self.build_task.busy() && self.plan_rev == self.rev && self.plan.is_some();
            ui.horizontal(|ui| {
                if ui.add_enabled(ok, egui::Button::new(egui::RichText::new(if self.update_build { "Update" } else { "Build" }).strong()).min_size(egui::vec2(120.0, 28.0))).clicked() {
                    // the plan made for the project as it is (the button waits for it)
                    let plan = self.plan.clone().unwrap_or_default();
                    let out = Path::new(self.out_dir.trim()).to_path_buf();
                    let (update, name, job) = (self.update_build, self.project.name.clone(), rage_toolkit_core::job::Job::new());
                    self.build_job = job.clone();
                    self.message = None;
                    self.build_task.run(move || {
                        let progress = |done: usize, all: usize| {
                            job.set_total(all);
                            if done > job.progress().0 {
                                job.step();
                            }
                        };
                        let mb = |b: u64| b as f64 / 1_048_576.0;
                        let textures = |s: &build::WriteStats| {
                            let mut t = String::new();
                            if s.shrunk > 0 {
                                t += &format!("; {} file(s) with smaller textures ({:.0} MB -> {:.0} MB)", s.shrunk, mb(s.bytes_before), mb(s.bytes_after));
                            }
                            if !s.copied_instead.is_empty() {
                                t += &format!("; {} could not be optimized and were copied (for example {})", s.copied_instead.len(), s.copied_instead[0]);
                            }
                            t
                        };
                        if update {
                            let r = build::build_update_with(&plan, &out, false, &progress)?;
                            reveal(&out.join(&plan.resource));
                            let stale = if r.stale.is_empty() { String::new() } else { format!("; {} file(s) there are no longer in the build (left as they are, for example {})", r.stale.len(), r.stale[0].display()) };
                            Ok(format!("{} file(s) written, {} unchanged{stale}{}", r.written, r.unchanged, textures(&r.stats)))
                        } else {
                            let (dir, stats) = build::build_with(&plan, &out, &progress)?;
                            reveal(&dir);
                            Ok(format!("Built {} file(s) in {}{}{}", plan.outputs.len(), dir.display(), if split { format!(" (add the lines of {name}.cfg to server.cfg)") } else { String::new() }, textures(&stats)))
                        }
                    });
                }
                if self.build_task.busy() {
                    let (d, t) = self.build_job.progress();
                    ui.add(egui::ProgressBar::new(if t > 0 { d as f32 / t as f32 } else { 0.0 }).desired_width(240.0).text(format!("{d}/{t} files")));
                }
            });
            if let Some(r) = self.build_task.poll(ctx) {
                self.message = Some(r);
            }
            if let Some(m) = &self.message {
                match m {
                    Ok(t) => ui.label(egui::RichText::new(t).color(crate::theme::ok())),
                    Err(t) => ui.label(egui::RichText::new(t).color(crate::theme::err())),
                };
            }
            ui.separator();
            if self.plan_rev != self.rev {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Planning the build (the YMTs are written and read back, every file is listed)...");
                });
            }
            if let Some(plan) = self.plan.as_ref().filter(|_| self.plan_rev == self.rev) {
                for i in plan.issues.iter().filter(|i| i.sev == Sev::Error && i.drawable.is_none()) {
                    ui.label(egui::RichText::new(format!("{}  ({})", i.message, i.fix)).color(crate::theme::err()));
                }
                if plan.outputs.is_empty() {
                    ui.weak("Nothing to build yet.");
                } else {
                    let sum: Vec<String> = plan.addons.iter().map(|(n, d, t)| format!("{n}: {d} drawables, {t} textures")).collect();
                    ui.label(format!("{} file(s)   [{}]", plan.outputs.len(), sum.join("; ")));
                    // per resource
                    let mut per: std::collections::BTreeMap<&str, usize> = Default::default();
                    for o in &plan.outputs {
                        let r = if plan.resource.is_empty() { o.dest.split_once('/').map(|x| x.0).unwrap_or("") } else { plan.resource.as_str() };
                        *per.entry(r).or_default() += 1;
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (r, n) in per.iter().filter(|x| !x.0.is_empty()) {
                            ui.label(egui::RichText::new(format!("{r}: {n}")).monospace().small());
                        }
                    });
                    // only the rows in view are laid out (a big pack has tens of thousands of files)
                    let row = ui.text_style_height(&egui::TextStyle::Body);
                    egui::ScrollArea::vertical().id_salt("cl_build_files").auto_shrink([false, false]).show_rows(ui, row, plan.outputs.len(), |ui, range| {
                        for o in &plan.outputs[range] {
                            let from = match &o.source {
                                build::Source::Copy(p) => clothing::file_of(p),
                                build::Source::Optimize { from, .. } => format!("{} (textures shrunk if over the size)", clothing::file_of(from)),
                                build::Source::Image(p) => format!("{} (made into a .ytd)", clothing::file_of(p)),
                                build::Source::Bytes(b) => format!("generated, {} bytes", b.len()),
                            };
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&o.dest).monospace());
                                ui.weak(from);
                            });
                        }
                    });
                }
            }
        });
        self.build_open = open;
    }
}

/// The page: the open packs as tabs (like other clothing tools' addons), each a [`Pack`]; items are copied or moved between them.
pub struct State {
    packs: Vec<Pack>,
    active: usize,
}

impl Default for State {
    fn default() -> Self {
        Self { packs: vec![Pack::default()], active: 0 }
    }
}

impl State {
    pub fn busy(&self) -> bool {
        self.packs.iter().any(Pack::busy)
    }

    pub fn add_dropped(&mut self, paths: &[PathBuf]) -> bool {
        self.packs[self.active].add_dropped(paths)
    }

    /// `--clothing-folder DIR` (automated screenshots): a built pack (caret names inside) opens as a pack, other folders are dropped.
    pub fn open_folder(&mut self, dir: &Path) {
        let built = clothing::scan(&[dir.to_path_buf()]).items.iter().any(|i| clothing::file_of(&i.path).contains('^'));
        if built {
            self.open_pack(dir);
        } else {
            self.add_dropped(&[dir.to_path_buf()]);
        }
    }

    /// `--clothing-find NAME` / `--clothing-layout` (automated screenshots): select the first drawable called NAME
    /// (`p_head_000`, or `female/feet_003`), open the pack layout window
    pub fn select_named(&mut self, name: &str, layout: bool) {
        let pack = &mut self.packs[self.active];
        let (sex, name) = match name.split_once('/') {
            Some((s, n)) => (Some(if s == "female" { Sex::Female } else { Sex::Male }), n),
            None => (None, name),
        };
        if let Some(d) = pack.project.drawables.iter().find(|d| format!("{}_{:03}", d.kind.name(), d.number) == name && sex.is_none_or(|s| s == d.sex)) {
            pack.selected.clear();
            pack.selected.insert(key(d));
        }
        pack.layout_open = layout;
    }

    /// Open a built pack (a FiveM resource or its stream folder) in a new tab; its files are only read.
    fn open_pack(&mut self, dir: &Path) {
        // a built pack (one resource, or a folder of them: control + streams) with every setting; a bare stream folder by its names
        let opened = clothing::import::import_tree(dir, None).map(|im| (im.project, im.notes)).or_else(|_| clothing::open_pack(dir).map(|p| (p, vec![])));
        match opened {
            Ok((project, notes)) => {
                let n = project.drawables.len();
                let dlcs = project.dlcs.len();
                let mut p = Pack { project, notes, ..Default::default() };
                p.message = Some(Ok(format!("Opened {n} drawable(s) in {dlcs} DLC(s) from {}. The pack's files are only read; Build writes a new resource.", dir.display())));
                if let Some(d) = p.project.drawables.first() {
                    p.selected.insert(key(d));
                }
                self.packs.push(p);
                self.active = self.packs.len() - 1;
            }
            Err(e) => self.packs[self.active].message = Some(Err(format!("{e:#}"))),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, render: Option<&eframe::egui_wgpu::RenderState>, game: Option<&Game>) {
        // the tabs
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (k, p) in self.packs.iter().enumerate() {
                let label = format!("{}{}", p.project.name, if p.dirty { " *" } else { "" });
                if ui.selectable_label(k == self.active, egui::RichText::new(label).strong()).clicked() {
                    self.active = k;
                }
                if self.packs.len() > 1 && k == self.active && ui.small_button("x").on_hover_text("Close this pack (save it first: unsaved changes are lost)").clicked() {
                    close = Some(k);
                }
            }
            ui.separator();
            if ui.button("+ New pack").clicked() {
                let mut p = Pack::default();
                p.project.name = format!("pack{}", self.packs.len() + 1);
                self.packs.push(p);
                self.active = self.packs.len() - 1;
            }
            if ui.button("Open pack folder...").on_hover_text("A built pack: a FiveM resource or its stream folder (mp_f_freemode_01_name^jbib_000_u.ydd ...). Only read; edit it here and Build a new one.").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.open_pack(&d);
                }
            }
        });
        if let Some(k) = close {
            self.packs.remove(k);
            self.active = self.active.min(self.packs.len() - 1);
        }
        let names: Vec<(usize, String)> = self.packs.iter().enumerate().map(|(k, p)| (k, p.project.name.clone())).collect();
        let a = self.active;
        self.packs[a].others = names.into_iter().filter(|(k, _)| *k != a).collect();
        self.packs[a].ui(ui, ctx, render, game);
        self.deliver(a);
    }

    /// Copies and moves from pack `from` to the others: each lands after the last of its sex and kind there.
    fn deliver(&mut self, from: usize) {
        let a = from;
        for (d, to) in std::mem::take(&mut self.packs[a].outbox) {
            if let Some(dest) = self.packs.get_mut(to) {
                // into the other pack's first DLC with room, as a drawable of its own there
                let n = dest.project.add_next(Drawable { dlc: 0, id: clothing::new_id(), ..d.clone() });
                dest.dirty = true;
                dest.rev += 1;
                dest.plan = None;
                let msg = format!("{} {}_{:03} went to {} as {}_{n:03}", d.sex.label(), d.kind.name(), d.number, dest.project.name, d.kind.name());
                dest.message = Some(Ok(msg.clone()));
                self.packs[a].message = Some(Ok(msg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawable(kind: Kind, number: u32, tex: &str) -> Drawable {
        Drawable { sex: Sex::Female, kind, number, source_number: number, ydd: PathBuf::from(format!("{number}.ydd")), race: "u".into(), textures: vec![Texture { path: PathBuf::from(tex), letter: 'a', race: "uni".into(), tex_id: None, edit: None }], ..Default::default() }
    }

    #[test]
    fn items_move_and_copy_between_packs_and_removed_ones_are_kept() {
        let mut page = State::default();
        let jbib = Kind::Component(11);
        page.packs[0].project.drawables = vec![drawable(jbib, 0, "a.ytd"), drawable(jbib, 1, "b.ytd")];
        let mut second = Pack::default();
        second.project.drawables = vec![drawable(jbib, 0, "x.ytd")];
        page.packs.push(second);
        // copy 001 to the second pack: it lands at 001 there, the source keeps it
        let d = page.packs[0].project.drawables[1].clone();
        page.packs[0].outbox.push((d, 1));
        page.deliver(0);
        assert_eq!(page.packs[0].project.drawables.len(), 2);
        let landed = page.packs[1].project.drawables.iter().find(|x| x.number == 1).unwrap();
        assert_eq!(landed.textures[0].path, PathBuf::from("b.ytd"), "the textures travel with the model");
        // Remove keeps it in the archive with its textures
        page.packs[0].selected.insert((0, Sex::Female, jbib, 0));
        page.packs[0].archive_selected();
        assert_eq!((page.packs[0].project.drawables.len(), page.packs[0].project.archived.len()), (1, 1));
        assert_eq!(page.packs[0].project.archived[0].textures[0].path, PathBuf::from("a.ytd"));
    }
}

