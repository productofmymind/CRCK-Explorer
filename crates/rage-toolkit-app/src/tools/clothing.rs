//! Clothing Pack Creator: drop .ydd models and .ytd textures (files or whole folders), see them paired and grouped by component,
//! fix the numbering, check them, preview what will be written, and build the FiveM resource.
//! The work is `rage_toolkit_core::clothing`; this page only shows it and asks the user.
use super::*;
use eframe::egui::Color32;
use rage_toolkit_core::clothing::{self, build, project as cproj, Drawable, Kind, OnConflict, Project, Sev, Sex, Texture};
use std::collections::BTreeSet;

type Key = (Sex, Kind, u32);

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
    checked: Option<Checked>,
    /// the 3D preview: on or off, the texture shown (index into the drawable's textures), the body around it
    preview_on: bool,
    preview: super::cloth_preview::Preview,
    preview_tex: usize,
    preview_body: bool,
    /// the drawables list's search box
    search: String,
    checks_open: bool,
    build_open: bool,
    /// a removed (archived) drawable shown in the card instead of the selection
    archived_sel: Option<usize>,
    /// the Move to slot boxes: sex, kind, number (filled from the selected drawable)
    slot: Option<(Key, Sex, Kind, u32)>,
    confirm_delete: bool,
    /// the other open packs (index, name), for Copy to / Move to; set by the page every frame
    others: Vec<(usize, String)>,
    /// drawables to add to another pack (its index): done by the page after this pack is drawn
    outbox: Vec<(Drawable, usize)>,
}

/// The checks and the project they were made for. They read the files (missing ones, duplicate texture content), so
/// they run again when the project changes, and every few seconds for changes on disk; not on every frame.
struct Checked {
    project: Project,
    at: std::time::Instant,
    issues: std::rc::Rc<Vec<clothing::Issue>>,
}

impl Default for Pack {
    fn default() -> Self {
        Self { project: Project::default(), file: None, dirty: false, selected: BTreeSet::new(), anchor: None, default_sex: Sex::Male, pending: None, pending_note: String::new(), held: vec![], notes: vec![], plan: None, out_dir: String::new(), message: None, checked: None, preview_on: true, preview: Default::default(), preview_tex: 0, preview_body: true, search: String::new(), checks_open: false, build_open: false, archived_sel: None, slot: None, confirm_delete: false, others: vec![], outbox: vec![] }
    }
}

/// a file name without a built pack's `mp_f_freemode_01_pack^` prefix
fn short(p: &Path) -> String {
    let f = clothing::file_of(p);
    f.rsplit('^').next().unwrap_or(&f).to_string()
}

fn key(d: &Drawable) -> Key {
    (d.sex, d.kind, d.number)
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
        self.preview_on && self.preview.loading()
    }

    /// The project's checks (see [`Checked`]).
    fn issues(&mut self) -> std::rc::Rc<Vec<clothing::Issue>> {
        let fresh = self.checked.as_ref().is_some_and(|c| c.at.elapsed() < std::time::Duration::from_secs(3) && c.project == self.project);
        if !fresh {
            self.checked = Some(Checked { project: self.project.clone(), at: std::time::Instant::now(), issues: std::rc::Rc::new(clothing::validate(&self.project)) });
        }
        self.checked.as_ref().map(|c| c.issues.clone()).unwrap_or_default()
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
        self.project_bar(ui);
        if self.pending.is_some() {
            self.conflict_bar(ui);
        }
        for n in &self.notes {
            ui.label(egui::RichText::new(n).color(crate::theme::warn()).small());
        }
        if !self.held.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("{} drawable(s) are held back for a second addon.", self.held.len())).color(crate::theme::info()));
                if ui.button("Start a new project with them").on_hover_text("Replaces this project: save it first").clicked() {
                    let held = std::mem::take(&mut self.held);
                    let name = format!("{}_02", self.project.name);
                    self.project = Project { archived: vec![], sorted: false, name, limit: self.project.limit, drawables: held };
                    self.project.close_gaps();
                    self.file = None;
                    self.selected.clear();
                    self.dirty = true;
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
        self.build_window(ctx);
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
                self.plan = None;
            }
            if ui.add_enabled(has, egui::Button::new("Re-letter")).on_hover_text("a, b, c, ... in the current order").clicked() {
                self.project.relabel_textures();
                self.dirty = true;
                self.plan = None;
            }
        });
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 34.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            {
                let big = |t: &str| egui::Button::new(egui::RichText::new(t).strong().size(14.0)).min_size(egui::vec2(110.0, 30.0));
                if ui.add(big("BUILD")).on_hover_text("Write the FiveM resource: the YMT, the shop meta, the manifest and the streamed files").clicked() {
                    self.build_open = true;
                    self.plan = Some(build::plan_build(&self.project));
                }
                ui.toggle_value(&mut self.preview_on, egui::RichText::new("3D PREVIEW").strong().size(14.0)).on_hover_text("Show the selected drawable on the freemode body, with the texture chosen in its list");
                let issues = self.issues();
                let (e, w) = (issues.iter().filter(|i| i.sev == Sev::Error).count(), issues.iter().filter(|i| i.sev == Sev::Warning).count());
                let (text, colour) = if e + w == 0 { ("no problems".to_string(), crate::theme::ok()) } else { (format!("{e} error(s), {w} warning(s)"), if e > 0 { crate::theme::err() } else { crate::theme::warn() }) };
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
        egui::ScrollArea::vertical().id_salt("cl_list").auto_shrink([false, false]).show(ui, |ui| {
            let mut last_group: Option<(Sex, Kind)> = None;
            for (i, d) in self.project.drawables.iter().enumerate() {
                let name = format!("{}_{:03}", d.kind.name(), d.number);
                if !find.is_empty() && !name.contains(&find) && !clothing::file_of(&d.ydd).to_lowercase().contains(&find) {
                    continue;
                }
                if last_group != Some((d.sex, d.kind)) {
                    if last_group.map(|g| g.0) != Some(d.sex) {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(d.sex.label().to_uppercase()).color(crate::theme::accent()).strong());
                    }
                    last_group = Some((d.sex, d.kind));
                    ui.label(egui::RichText::new(format!("{}  ({})", d.kind.name(), self.project.count(d.sex, d.kind))).color(crate::theme::dim()));
                }
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::click());
                let on = self.selected.contains(&key(d));
                let p = ui.painter();
                p.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 5.0, if on { ui.visuals().selection.bg_fill } else if resp.hovered() { crate::theme::surface_hi() } else { crate::theme::surface() });
                // a coloured edge for the sex, a dot for the state, like other clothing tools
                p.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 1.0), egui::vec2(3.0, rect.height() - 2.0)), 2.0, if d.sex == Sex::Female { Color32::from_rgb(230, 90, 150) } else { Color32::from_rgb(80, 150, 240) });
                let dot = match worst[i] {
                    Some(Sev::Error) => crate::theme::err(),
                    Some(Sev::Warning) => crate::theme::warn(),
                    _ => crate::theme::ok(),
                };
                p.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, dot);
                let renumbered = if d.source_number != d.number { format!("  (was {:03})", d.source_number) } else { String::new() };
                p.text(egui::pos2(rect.left() + 26.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("{name}{renumbered}"), egui::FontId::proportional(14.0), crate::theme::text());
                let tex = format!("{} tex{}", d.textures.len(), if d.cloth.is_some() { ", cloth" } else { "" });
                p.text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, tex, egui::FontId::proportional(12.0), crate::theme::dim());
                if resp.clicked() {
                    clicked = Some(key(d));
                }
            }
            if !self.project.archived.is_empty() {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("REMOVED ({})", self.project.archived.len())).color(crate::theme::warn()).strong()).on_hover_text("Taken out of the pack, kept with their files. Click one to restore it, move it to another pack or delete it.");
                for (j, d) in self.project.archived.iter().enumerate() {
                    let on = self.archived_sel == Some(j);
                    let r = ui.selectable_label(on, egui::RichText::new(format!("{} {}_{:03}", d.sex.label(), d.kind.name(), d.number)).color(crate::theme::dim()));
                    if r.clicked() {
                        archived_click = Some(j);
                    }
                }
            }
            if self.project.drawables.is_empty() {
                ui.add_space(30.0);
                ui.label(egui::RichText::new("Drop .ydd models and their textures (.ytd or images) here, or use + FEMALE / + MALE below.").color(crate::theme::dim()));
            }
        });
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
            // the drawable is found again by its model file, because swapping changes the numbers
            let (sex, kind, ydd) = (k.0, k.1, self.project.drawables[i].ydd.clone());
            self.project.move_drawable(i, by);
            if let Some(d) = self.project.drawables.iter().find(|d| d.sex == sex && d.kind == kind && d.ydd == ydd) {
                now.insert(key(d));
            }
        }
        self.selected = now;
        self.dirty = true;
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

    /// Move to slot: the drawable's sex, type and number, with every file following (names change at build).
    fn slot_editor(&mut self, ui: &mut egui::Ui, i: usize) {
        let d = &self.project.drawables[i];
        let k = key(d);
        if self.slot.as_ref().map(|s| s.0) != Some(k) {
            self.slot = Some((k, d.sex, d.kind, d.number));
        }
        let Some((_, mut sex, mut kind, mut number)) = self.slot else { return };
        let prop = d.kind.is_prop();
        ui.horizontal_wrapped(|ui| {
            ui.label("Move to slot");
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
            let taken = self.project.drawables.iter().any(|x| (x.sex, x.kind, x.number) == (sex, kind, number) && key(x) != k);
            let same = (sex, kind, number) == (k.0, k.1, k.2);
            let tip = if taken { "That slot is taken: the two swap (or the other one moves to its group's next free number)" } else { "Its model, textures and cloth go with it; their file names follow when the pack is built" };
            if ui.add_enabled(!same, egui::Button::new(if taken { "Swap" } else { "Move" })).on_hover_text(tip).clicked() {
                match self.project.move_to(i, sex, kind, number) {
                    Ok(()) => {
                        self.selected.clear();
                        self.selected.insert((sex, kind, number));
                        self.changed(format!("Moved to {} {}_{number:03}", sex.label(), kind.name()));
                    }
                    Err(e) => self.message = Some(Err(e)),
                }
            }
        });
        self.slot = self.slot.map(|s| (s.0, sex, kind, number));
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
            });
        });
        let shown = match self.archived_sel {
            Some(j) => self.project.archived.get(j),
            None => self.single().map(|i| &self.project.drawables[i]),
        };
        let want = shown.map(|d| {
            super::cloth_preview::Want { ydd: d.ydd.clone(), texture: d.textures.get(self.preview_tex).or(d.textures.first()).map(|t| t.path.clone()), sex: d.sex, kind: d.kind, body: self.preview_body }
        });
        self.preview.ui(ui, render, want, game);
    }

    /// Middle: the selected drawable, like other clothing tools' card: what it is, its files, its textures, its problems.
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
        self.slot_editor(ui, i);
        let Some(i) = self.single() else { return };
        let this = self.project.drawables[i].clone();
        ui.horizontal_wrapped(|ui| {
            self.send_menu(ui, &this, true);
            if self.send_menu(ui, &this, false) {
                self.project.drawables.remove(i);
                self.selected.clear();
                self.changed("Moved to the other pack".into());
            }
        });
        let Some(i) = self.single() else { return };
        ui.add_space(4.0);
        let mut changed = false;
        let mut archive_it = false;
        let d = &mut self.project.drawables[i];
        let field = |ui: &mut egui::Ui, label: &str, value: String| {
            egui::Frame::group(ui.style()).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_min_width(100.0);
                ui.label(egui::RichText::new(label).small().color(crate::theme::dim()));
                ui.label(egui::RichText::new(value).strong());
            });
        };
        ui.horizontal_wrapped(|ui| {
            field(ui, "Drawable", format!("{}_{:03}", d.kind.name(), d.number));
            field(ui, "Drawable type", d.kind.name().to_string());
            field(ui, "Sex", d.sex.label().to_string());
            if d.source_number != d.number {
                field(ui, "Its file was numbered", format!("{:03}", d.source_number));
            }
        });
        ui.add_space(8.0);
        let file_row = |ui: &mut egui::Ui, label: &str, path: Option<&PathBuf>| {
            ui.label(egui::RichText::new(label).color(crate::theme::dim()));
            match path {
                // a built pack's caret names are long: the part after the caret, the whole path on hover
                Some(p) => ui.label(egui::RichText::new(short(p)).color(if p.is_file() { crate::theme::text() } else { crate::theme::err() })).on_hover_text(p.display().to_string()),
                None => ui.weak("none"),
            };
        };
        egui::Grid::new("cl_files").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
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
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Textures ({})", d.textures.len())).strong()).on_hover_text("The one picked (the round button) is shown in the 3D preview");
        });
        let mut remove = None;
        egui::Frame::group(ui.style()).corner_radius(6.0).show(ui, |ui| {
            ui.set_min_height(140.0);
            ui.set_width(ui.available_width());
            for (n, t) in d.textures.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.preview_tex, n, egui::RichText::new(t.letter.to_string()).monospace().strong()).on_hover_text("Show this one in the 3D preview");
                    ui.label(egui::RichText::new(short(&t.path)).color(if t.path.is_file() { crate::theme::text() } else { crate::theme::err() })).on_hover_text(t.path.display().to_string());
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
                        d.textures.push(Texture { path: f, letter, race: String::new() });
                    }
                    changed = true;
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
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Problems").strong());
            for x in mine {
                ui.label(egui::RichText::new(&x.message).color(sev_colour(x.sev)));
                ui.weak(format!("How to fix: {}", x.fix));
            }
        }
        if changed {
            self.dirty = true;
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

    /// Where to write, what will be written, and the Build button.
    fn build_window(&mut self, ctx: &egui::Context) {
        if !self.build_open {
            return;
        }
        let errors = self.issues().iter().filter(|i| i.sev == Sev::Error).count();
        let mut open = true;
        egui::Window::new("Build the FiveM resource").open(&mut open).default_size([680.0, 420.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Write into");
                ui.add(egui::TextEdit::singleline(&mut self.out_dir).desired_width(360.0).hint_text("a folder; the pack goes in <folder>/<pack name>"));
                if ui.button("Browse...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.out_dir = d.display().to_string();
                    }
                }
            });
            if ui.checkbox(&mut self.project.sorted, "Sort the streamed files into folders by category").on_hover_text("On: stream/female/jbib/, stream/male/lowr/, stream/male/p_head/ ... Off: everything in stream/. FiveM streams both the same way; it is only how the pack is laid out.").changed() {
                self.dirty = true;
                self.plan = None;
            }
            if errors > 0 {
                ui.label(egui::RichText::new(format!("{errors} error(s) to fix first (the checks at the bottom of the page)")).color(crate::theme::err()));
            }
            let ok = !self.out_dir.trim().is_empty() && errors == 0 && !self.project.drawables.is_empty();
            if ui.add_enabled(ok, egui::Button::new(egui::RichText::new("Build").strong()).min_size(egui::vec2(120.0, 28.0))).clicked() {
                let plan = build::plan_build(&self.project);
                self.message = Some(match build::build(&plan, Path::new(self.out_dir.trim())) {
                    Ok(dir) => {
                        reveal(&dir);
                        Ok(format!("Built {} file(s) in {}", plan.outputs.len(), dir.display()))
                    }
                    Err(e) => Err(format!("{e:#}")),
                });
                self.plan = Some(plan);
            }
            if let Some(m) = &self.message {
                match m {
                    Ok(t) => ui.label(egui::RichText::new(t).color(crate::theme::ok())),
                    Err(t) => ui.label(egui::RichText::new(t).color(crate::theme::err())),
                };
            }
            ui.separator();
            if self.plan.is_none() {
                self.plan = Some(build::plan_build(&self.project));
            }
            if let Some(plan) = &self.plan {
                for i in plan.issues.iter().filter(|i| i.sev == Sev::Error && i.drawable.is_none()) {
                    ui.label(egui::RichText::new(format!("{}  ({})", i.message, i.fix)).color(crate::theme::err()));
                }
                if plan.outputs.is_empty() {
                    ui.weak("Nothing to build yet.");
                } else {
                    let sum: Vec<String> = plan.addons.iter().map(|(n, d, t)| format!("{n}: {d} drawables, {t} textures")).collect();
                    ui.label(format!("{} file(s) in {}/   [{}]", plan.outputs.len(), plan.resource, sum.join("; ")));
                    egui::ScrollArea::vertical().id_salt("cl_build_files").auto_shrink([false, false]).show(ui, |ui| {
                        for o in &plan.outputs {
                            let from = match &o.source {
                                build::Source::Copy(p) => clothing::file_of(p),
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

    /// Open a built pack (a FiveM resource or its stream folder) in a new tab; its files are only read.
    fn open_pack(&mut self, dir: &Path) {
        match clothing::open_pack(dir) {
            Ok(project) => {
                let n = project.drawables.len();
                let mut p = Pack { project, ..Default::default() };
                p.message = Some(Ok(format!("Opened {} drawable(s) from {}. The pack's files are only read; Build writes a new resource.", n, dir.display())));
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
                let n = dest.project.add_next(d.clone());
                dest.dirty = true;
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
        Drawable { sex: Sex::Female, kind, number, source_number: number, ydd: PathBuf::from(format!("{number}.ydd")), race: "u".into(), cloth: None, alternates: vec![], textures: vec![Texture { path: PathBuf::from(tex), letter: 'a', race: "uni".into() }] }
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
        page.packs[0].selected.insert((Sex::Female, jbib, 0));
        page.packs[0].archive_selected();
        assert_eq!((page.packs[0].project.drawables.len(), page.packs[0].project.archived.len()), (1, 1));
        assert_eq!(page.packs[0].project.archived[0].textures[0].path, PathBuf::from("a.ytd"));
    }
}
