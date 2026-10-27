//! The archetype editor: a `.ytyp` opened from the Explorer in a floating window. A searchable list of its archetypes, an inspector
//! built from the game's own structure tables (so base, time and MLO archetypes, their rooms, portals, entities and extensions all
//! show the members they really have), undo / redo, checks with how to fix each, the XML of the selected archetype, and safe saving.
//! The work is `rage_toolkit_core::archetype`; this file only shows it and relays what the user does.
use eframe::egui::{self, Color32};
use rage_toolkit_core::archetype::{
    MetaValue,
    document::{archetype_path, ArchKind, YtypDocument},
    edit::{Command, Path as VPath, Step},
    merge::{self, Choice, MergePlan, MergeReport, Status},
    schema_view::{self, FieldDesc, FieldKind},
    validate::{validate, AssetIndex, Issue, Sev},
};
use rage_toolkit_core::metaxml::{self, Report, Severity};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
};


/// What the user did in the inspector this frame; applied after it is drawn (the tree is borrowed while drawing).
enum Act {
    Text(VPath, FieldKind, String),
    Value(VPath, MetaValue, String),
    Insert(VPath, usize, MetaValue),
    Remove(VPath, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Fields,
    Xml,
}

/// The review of another file's archetypes before they are merged into the open one.
struct MergeState {
    path: PathBuf,
    other: YtypDocument,
    plan: MergePlan,
    choices: Vec<Choice>,
    with_extra: bool,
    /// what the open file would have after the merge: (key it was computed for, errors, warnings, report) or why it cannot be done
    preview: Option<(String, Result<(usize, usize, MergeReport), String>)>,
}

impl MergeState {
    fn preview(&mut self, base: &YtypDocument, index: Option<&AssetIndex>) {
        let key = format!("{:?}{}{}{}", self.choices, self.with_extra, base.undo_steps(), base.len());
        if self.preview.as_ref().map(|p| &p.0) == Some(&key) {
            return;
        }
        let mut fork = base.fork();
        let r = merge::apply(&mut fork, &self.other, &self.plan, &self.choices, self.with_extra).map_err(|e| format!("{e:#}")).map(|rep| {
            let issues = validate(&fork, index);
            (issues.iter().filter(|i| i.sev == Sev::Error).count(), issues.iter().filter(|i| i.sev == Sev::Warning).count(), rep)
        });
        self.preview = Some((key, r));
    }
}

/// The XML of the selected archetype being edited as text, with the syntax check of what is typed.
struct XmlEdit {
    /// what it was loaded for: (history steps, archetype, archetype count)
    key: (usize, usize, usize),
    text: String,
    original: String,
    report: Report,
    /// the text the report is for
    checked: String,
    jump: Option<usize>,
}

struct Win {
    id: u64,
    merge: Option<MergeState>,
    xml_edit: Option<XmlEdit>,
    doc: YtypDocument,
    /// opened from a game archive: a cached copy, only "Save as"
    from_game: bool,
    selected: Option<usize>,
    filter: String,
    tab: Tab,
    show_notes: bool,
    issues: Vec<Issue>,
    /// the history length the issues were computed at (and whether the asset index was there)
    checked: Option<(usize, bool, usize)>,
    /// text being typed in the inspector, by path
    buffers: HashMap<String, String>,
    message: Option<Result<String, String>>,
    new_name: String,
    /// how many asset files the names were last learned from
    learned: usize,
}

#[derive(Default)]
pub struct Ytyps {
    open: Vec<Win>,
    next: u64,
    /// models and dictionaries of the game (and added folders), built on a thread
    index: Option<Arc<AssetIndex>>,
    job: Option<mpsc::Receiver<AssetIndex>>,
    extra_folders: Vec<PathBuf>,
}

impl Ytyps {
    pub fn open(&mut self, path: &Path, from_game: bool) -> Result<(), String> {
        if self.open.iter().any(|w| w.doc.path.as_deref() == Some(path)) {
            return Ok(());
        }
        let doc = YtypDocument::open_file(path).map_err(|e| format!("{}: {e:#}", path.display()))?;
        self.push(doc, from_game);
        Ok(())
    }

    pub fn open_new(&mut self) {
        self.push(YtypDocument::new_file("my_props"), false);
    }

    fn push(&mut self, mut doc: YtypDocument, from_game: bool) {
        self.next += 1;
        // show the archetype names as text: the file's own names are not in the game's table, so the ones typed are added as we go
        let _ = &mut doc;
        self.open.push(Win { id: self.next, merge: None, xml_edit: None, selected: if doc.is_empty() { None } else { Some(0) }, doc, from_game, filter: String::new(), tab: Tab::Fields, show_notes: false, issues: vec![], checked: None, buffers: HashMap::new(), message: None, new_name: String::new(), learned: 0 });
    }

    /// Start building the index of the game's models and texture dictionaries (and added folders) on a thread.
    pub fn index_game(&mut self, game: Option<&rage_toolkit_core::game::Game>) {
        let (tx, rx) = mpsc::channel();
        let folders = self.extra_folders.clone();
        let game_dir = game.map(|g| g.dir.clone());
        self.job = Some(rx);
        std::thread::spawn(move || {
            let mut ix = match game_dir.as_deref().and_then(|d| rage_toolkit_core::game::Game::open(Some(d)).ok()) {
                Some(g) => AssetIndex::from_game(&g),
                None => AssetIndex::default(),
            };
            for f in &folders {
                ix.add_folder(f);
            }
            let _ = tx.send(ix);
        });
    }

    #[allow(deprecated)] // Id::new is the plain way to key a window by a number
    pub fn show(&mut self, ctx: &egui::Context, game: Option<&rage_toolkit_core::game::Game>) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(ix) => {
                    self.index = Some(Arc::new(ix));
                    self.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
                Err(_) => self.job = None,
            }
        }
        let busy = self.job.is_some();
        let index = self.index.clone();
        let mut want_index = false;
        let mut add_folder: Option<PathBuf> = None;
        self.open.retain_mut(|w| {
            let mut open = true;
            let title = format!("{}{}", w.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "new archetype file".into()), if w.doc.is_dirty() { "  *" } else { "" });
            egui::Window::new(title).id(egui::Id::new(("ytyp", w.id))).open(&mut open).default_size([1000.0, 680.0]).resizable(true).show(ctx, |ui| {
                let r = w.ui(ui, index.as_deref(), busy);
                want_index |= r.0;
                if add_folder.is_none() {
                    add_folder = r.1;
                }
            });
            open
        });
        if let Some(f) = add_folder {
            self.extra_folders.push(f);
            want_index = true;
        }
        if want_index && !busy {
            self.index_game(game);
        }
    }
}

fn kind_tag(k: ArchKind) -> &'static str {
    match k {
        ArchKind::Base => "prop",
        ArchKind::Time => "time",
        ArchKind::Mlo => "MLO ",
        ArchKind::Other => "?   ",
    }
}

impl Win {
    /// Returns (asked for the asset index, a folder to add to it).
    fn ui(&mut self, ui: &mut egui::Ui, index: Option<&AssetIndex>, indexing: bool) -> (bool, Option<PathBuf>) {
        let mut want_index = false;
        let mut folder = None;
        // an archetype is usually called like its model: the model names of the game turn the hashes back into names
        if let Some(ix) = index.filter(|ix| ix.files != self.learned) {
            for n in ix.names() {
                self.doc.names.add_lowercase(n);
            }
            self.learned = ix.files;
            self.checked = None;
        }
        self.refresh_issues(index);
        self.toolbar(ui, index, indexing, &mut want_index, &mut folder);
        if !self.doc.read_warnings.is_empty() {
            ui.label(egui::RichText::new(format!("{} part(s) of this file could not be decoded and are kept as they were (first: {})", self.doc.read_warnings.len(), self.doc.read_warnings[0])).color(crate::theme::warn()).small());
        }
        if let Some(m) = &self.message {
            match m {
                Ok(t) => ui.label(egui::RichText::new(t).color(crate::theme::ok())),
                Err(t) => ui.label(egui::RichText::new(t).color(crate::theme::err())),
            };
        }
        ui.separator();
        if self.merge.is_some() {
            self.merge_ui(ui, index);
            return (want_index, folder);
        }
        let issues_h = if self.issues.iter().any(|i| self.show_notes || i.sev != Sev::Info) { 130.0 } else { 24.0 };
        let body_h = (ui.available_height() - issues_h - 16.0).max(200.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(280.0, body_h), egui::Layout::top_down(egui::Align::LEFT), |ui| self.list(ui, body_h));
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), body_h), egui::Layout::top_down(egui::Align::LEFT), |ui| self.inspector(ui, body_h));
        });
        ui.separator();
        self.issues_panel(ui);
        (want_index, folder)
    }

    fn refresh_issues(&mut self, index: Option<&AssetIndex>) {
        let key = (self.doc.undo_steps(), index.is_some(), self.doc.len());
        if self.checked != Some(key) {
            self.issues = validate(&self.doc, index);
            self.checked = Some(key);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, index: Option<&AssetIndex>, indexing: bool, want_index: &mut bool, folder: &mut Option<PathBuf>) {
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.doc.is_dirty() && !self.from_game && self.doc.path.is_some(), egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked() {
                self.save(None);
            }
            if ui.button("Save as...").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("Archetype file", &["ytyp"]).set_file_name(self.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "props.ytyp".into())).save_file() {
                    self.save(Some(f));
                }
            }
            ui.separator();
            let (ul, rl) = (self.doc.undo_label().map(str::to_string), self.doc.redo_label().map(str::to_string));
            if ui.add_enabled(self.doc.can_undo(), egui::Button::new("Undo")).on_hover_text(ul.map(|l| format!("{l} (Ctrl+Z)")).unwrap_or_default()).clicked() {
                self.undo();
            }
            if ui.add_enabled(self.doc.can_redo(), egui::Button::new("Redo")).on_hover_text(rl.map(|l| format!("{l} (Ctrl+Y)")).unwrap_or_default()).clicked() {
                self.redo();
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.new_name).desired_width(130.0).hint_text("name"));
            let named = !self.new_name.trim().is_empty();
            ui.menu_button("Add", |ui| {
                for (k, label) in [(ArchKind::Base, "Prop / building (archetype)"), (ArchKind::Time, "Time archetype"), (ArchKind::Mlo, "MLO (interior)")] {
                    if ui.add_enabled(named, egui::Button::new(label)).on_disabled_hover_text("type a name first").clicked() {
                        match self.doc.add_archetype(k, &self.new_name) {
                            Ok(i) => {
                                self.selected = Some(i);
                                self.new_name.clear();
                            }
                            Err(e) => self.message = Some(Err(format!("{e:#}"))),
                        }
                        ui.close();
                    }
                }
            });
            if ui.add_enabled(self.selected.is_some() && named, egui::Button::new("Duplicate as")).on_hover_text("copy the selected archetype under the name in the box").clicked() {
                if let Some(i) = self.selected {
                    match self.doc.duplicate_archetype(i, &self.new_name) {
                        Ok(n) => {
                            self.selected = Some(n);
                            self.new_name.clear();
                        }
                        Err(e) => self.message = Some(Err(format!("{e:#}"))),
                    }
                }
            }
            if ui.add_enabled(self.selected.is_some() && named, egui::Button::new("Rename to")).on_hover_text("give the selected archetype the name in the box").clicked() {
                if let Some(i) = self.selected {
                    match self.doc.rename_archetype(i, &self.new_name) {
                        Ok(()) => self.new_name.clear(),
                        Err(e) => self.message = Some(Err(format!("{e:#}"))),
                    }
                }
            }
            if ui.add_enabled(self.selected.is_some(), egui::Button::new("Remove")).clicked() {
                if let Some(i) = self.selected.take() {
                    if let Err(e) = self.doc.remove_archetype(i) {
                        self.message = Some(Err(format!("{e:#}")));
                    }
                }
            }
            if ui.button("Merge another .ytyp...").on_hover_text("Bring the archetypes of another file into this one, after you have reviewed each one").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("Archetype file", &["ytyp"]).pick_file() {
                    self.start_merge(&f, index);
                }
            }
            ui.separator();
            ui.checkbox(&mut self.show_notes, "Notes");
            let label = match index {
                Some(ix) => format!("Assets: {} files", ix.files),
                None if indexing => "Looking up assets...".to_string(),
                None => "Look up models in the game".to_string(),
            };
            if ui.add_enabled(!indexing, egui::Button::new(label)).on_hover_text("Checks that each archetype's model and texture dictionary exist in the game or in the folders you add").clicked() {
                *want_index = true;
            }
            if ui.button("Add a folder...").on_hover_text("a folder with your own .ydr / .yft / .ytd files, used by the asset lookup").clicked() {
                *folder = rfd::FileDialog::new().pick_folder();
            }
            if ui.button("Export XML...").on_hover_text("the whole file as the reference tool XML").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("XML", &["xml"]).set_file_name("archetypes.ytyp.xml").save_file() {
                    self.message = Some(std::fs::write(&f, self.doc.to_xml(None)).map(|_| format!("Exported to {}", f.display())).map_err(|e| e.to_string()));
                }
            }
        });
        if self.from_game {
            ui.weak("From the game: a cached copy. Use Save as... to keep your changes.");
        }
        // keyboard
        let (save, undo, redo) = ui.input(|i| (i.modifiers.command && i.key_pressed(egui::Key::S), i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z), i.modifiers.command && (i.key_pressed(egui::Key::Y) || (i.modifiers.shift && i.key_pressed(egui::Key::Z)))));
        if !ui.ctx().egui_wants_keyboard_input() {
            if save && self.doc.is_dirty() && !self.from_game && self.doc.path.is_some() {
                self.save(None);
            }
            if undo {
                self.undo();
            }
            if redo {
                self.redo();
            }
        }
    }

    fn start_merge(&mut self, path: &Path, index: Option<&AssetIndex>) {
        if self.doc.path.as_deref() == Some(path) {
            self.message = Some(Err("That is the file that is open: pick another one.".into()));
            return;
        }
        match YtypDocument::open_file(path) {
            Ok(mut other) => {
                if let Some(ix) = index {
                    for n in ix.names() {
                        other.names.add_lowercase(n);
                    }
                }
                let plan = merge::plan(&self.doc, &other);
                let choices = merge::default_choices(&plan);
                self.message = None;
                self.merge = Some(MergeState { path: path.to_path_buf(), other, plan, choices, with_extra: true, preview: None });
            }
            Err(e) => self.message = Some(Err(format!("{}: {e:#}", path.display()))),
        }
    }

    fn merge_ui(&mut self, ui: &mut egui::Ui, index: Option<&AssetIndex>) {
        let Some(m) = self.merge.as_mut() else { return };
        m.preview(&self.doc, index);
        let mut apply = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Merge").strong().size(16.0));
            ui.label(format!("{}  into  {}", m.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), self.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "this file".into())));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("{} new", m.plan.count(Status::New))).color(crate::theme::ok()));
            ui.label(egui::RichText::new(format!("{} identical", m.plan.count(Status::Identical))).color(crate::theme::dim()));
            ui.label(egui::RichText::new(format!("{} with a different version here", m.plan.count(Status::Conflict))).color(crate::theme::warn()));
            let rep = m.plan.count(Status::RepeatedInOther);
            if rep > 0 {
                ui.label(egui::RichText::new(format!("{rep} repeated in the other file")).color(crate::theme::warn()));
            }
        });
        ui.weak("Nothing changes here until you press Merge. Your own archetypes are never touched unless you choose Replace.");
        ui.horizontal(|ui| {
            if ui.small_button("Add all new").clicked() {
                m.choices = merge::default_choices(&m.plan);
            }
            if ui.small_button("Skip all").clicked() {
                m.choices = m.plan.entries.iter().map(|_| Choice::Skip).collect();
            }
            if ui.small_button("Replace all that differ").clicked() {
                for (e, c) in m.plan.entries.iter().zip(m.choices.iter_mut()) {
                    if e.status == Status::Conflict {
                        *c = Choice::Replace;
                    }
                }
            }
            if m.plan.extra_items > 0 {
                ui.checkbox(&mut m.with_extra, format!("also bring {} extension / composite entity item(s) the file has", m.plan.extra_items));
            }
        });
        ui.separator();
        let h = (ui.available_height() - 70.0).max(160.0);
        egui::ScrollArea::vertical().id_salt("yt_merge").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("yt_merge_grid").num_columns(5).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                for head in ["", "Archetype", "Type", "State", "What to do"] {
                    ui.label(egui::RichText::new(head).strong());
                }
                ui.end_row();
                for (e, c) in m.plan.entries.iter().zip(m.choices.iter_mut()) {
                    let (tag, col) = match e.status {
                        Status::New => ("new", crate::theme::ok()),
                        Status::Identical => ("same", crate::theme::dim()),
                        Status::Conflict => ("differs", crate::theme::warn()),
                        Status::RepeatedInOther => ("repeated", crate::theme::warn()),
                    };
                    ui.label(egui::RichText::new(if matches!(c, Choice::Skip) { " " } else if matches!(c, Choice::Replace) { "~" } else { "+" }).monospace().color(crate::theme::ok()));
                    ui.label(&e.name);
                    ui.label(egui::RichText::new(kind_tag(e.kind)).monospace().color(crate::theme::dim()));
                    let state = ui.label(egui::RichText::new(tag).color(col));
                    if e.status == Status::Conflict && !e.differing.is_empty() {
                        state.on_hover_text(format!("different: {}", e.differing.join(", ")));
                    }
                    ui.horizontal(|ui| {
                        let skip_label = if e.status == Status::Conflict { "Keep mine" } else { "Skip" };
                        let now = match c {
                            Choice::Add => "Add",
                            Choice::Skip => skip_label,
                            Choice::Replace => "Replace mine",
                            Choice::AddAs(_) => "Add as...",
                        };
                        egui::ComboBox::from_id_salt(("yt_merge_choice", e.index)).selected_text(now).width(110.0).show_ui(ui, |ui| {
                            if e.status == Status::New && ui.selectable_label(matches!(c, Choice::Add), "Add").clicked() {
                                *c = Choice::Add;
                            }
                            if ui.selectable_label(matches!(c, Choice::Skip), skip_label).clicked() {
                                *c = Choice::Skip;
                            }
                            if e.status == Status::Conflict && ui.selectable_label(matches!(c, Choice::Replace), "Replace mine").clicked() {
                                *c = Choice::Replace;
                            }
                            if ui.selectable_label(matches!(c, Choice::AddAs(_)), "Add as...").clicked() && !matches!(c, Choice::AddAs(_)) {
                                *c = Choice::AddAs(format!("{}_2", e.name));
                            }
                        });
                        if let Choice::AddAs(n) = c {
                            ui.add(egui::TextEdit::singleline(n).desired_width(150.0));
                        }
                        if e.status == Status::Conflict && !e.differing.is_empty() {
                            ui.weak(format!("differs in {}", e.differing.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        ui.separator();
        let blocked = match &m.preview {
            Some((_, Ok((e, w, rep)))) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("After merging: {} added, {} replaced, {} renamed, {} left out.", rep.added, rep.replaced, rep.renamed, rep.skipped));
                    if *e == 0 && *w == 0 {
                        ui.label(egui::RichText::new("The checks find no problems.").color(crate::theme::ok()));
                    } else {
                        ui.label(egui::RichText::new(format!("The checks would show {e} error(s) and {w} warning(s).")).color(if *e > 0 { crate::theme::err() } else { crate::theme::warn() }));
                    }
                });
                false
            }
            Some((_, Err(why))) => {
                ui.label(egui::RichText::new(why).color(crate::theme::err()));
                true
            }
            None => false,
        };
        ui.horizontal(|ui| {
            if ui.add_enabled(!blocked, egui::Button::new("Merge")).on_hover_text("One undo step; nothing is written to disk until you save").clicked() {
                apply = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
        if apply {
            if let Some(m) = self.merge.take() {
                match merge::apply(&mut self.doc, &m.other, &m.plan, &m.choices, m.with_extra) {
                    Ok(r) => {
                        self.message = Some(Ok(format!("Merged: {} added, {} replaced, {} renamed, {} left out. Undo takes it all back; save when you are happy.", r.added, r.replaced, r.renamed, r.skipped)));
                        self.buffers.clear();
                        self.checked = None;
                        if self.selected.is_none() && !self.doc.is_empty() {
                            self.selected = Some(0);
                        }
                    }
                    Err(e) => {
                        self.message = Some(Err(format!("{e:#}")));
                        self.merge = Some(m);
                    }
                }
            }
        } else if cancel {
            self.merge = None;
        }
    }

    fn save(&mut self, to: Option<PathBuf>) {
        let Some(p) = to.or_else(|| self.doc.path.clone()) else { return };
        match self.doc.save(&p) {
            Ok(()) => {
                self.from_game = false;
                self.message = Some(Ok(format!("Saved {}", p.display())));
            }
            Err(e) => self.message = Some(Err(format!("{e:#}"))),
        }
    }

    fn undo(&mut self) {
        if let Err(e) = self.doc.undo() {
            self.message = Some(Err(format!("{e:#}")));
        }
        self.buffers.clear();
        self.clamp_selection();
    }

    fn redo(&mut self) {
        if let Err(e) = self.doc.redo() {
            self.message = Some(Err(format!("{e:#}")));
        }
        self.buffers.clear();
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.selected.map(|i| i >= self.doc.len()).unwrap_or(false) {
            self.selected = self.doc.len().checked_sub(1);
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Archetypes ({})", self.doc.len())).strong());
        });
        ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(f32::INFINITY).hint_text("search by name"));
        let f = self.filter.trim().to_lowercase();
        let rows: Vec<usize> = (0..self.doc.len()).filter(|&i| f.is_empty() || self.doc.name(i).to_lowercase().contains(&f)).collect();
        let mut worst: HashMap<usize, Sev> = HashMap::new();
        for i in &self.issues {
            if let Some(a) = i.archetype {
                if self.show_notes || i.sev != Sev::Info {
                    let e = worst.entry(a).or_insert(i.sev);
                    *e = (*e).min(i.sev);
                }
            }
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().id_salt("yt_list").max_height(height - 60.0).auto_shrink([false, false]).show_rows(ui, 20.0, rows.len(), |ui, range| {
            for pos in range {
                let i = rows[pos];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
                let on = self.selected == Some(i);
                if on {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, crate::theme::surface_hi());
                }
                let (mark, col) = match worst.get(&i) {
                    Some(Sev::Error) => ("!", crate::theme::err()),
                    Some(Sev::Warning) => ("!", crate::theme::warn()),
                    Some(Sev::Info) => ("i", crate::theme::info()),
                    None => (" ", crate::theme::dim()),
                };
                ui.painter().text(egui::pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, mark, egui::FontId::monospace(13.0), col);
                ui.painter().text(egui::pos2(rect.left() + 22.0, rect.center().y), egui::Align2::LEFT_CENTER, kind_tag(self.doc.kind(i)), egui::FontId::monospace(12.0), crate::theme::dim());
                ui.painter().text(egui::pos2(rect.left() + 62.0, rect.center().y), egui::Align2::LEFT_CENTER, self.doc.name(i), egui::FontId::proportional(14.0), if on { crate::theme::text() } else { crate::theme::text() });
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = clicked {
            if self.selected != Some(i) {
                self.buffers.clear();
            }
            self.selected = Some(i);
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Fields, "Properties");
            ui.selectable_value(&mut self.tab, Tab::Xml, "XML");
        });
        let Some(i) = self.selected.filter(|i| *i < self.doc.len()) else {
            ui.add_space(20.0);
            ui.weak(if self.doc.is_empty() { "This file has no archetypes yet: type a name and use Add." } else { "Select an archetype on the left." });
            return;
        };
        match self.tab {
            Tab::Xml => self.xml_tab(ui, i, height),
            Tab::Fields => {
                let mut acts: Vec<Act> = vec![];
                egui::ScrollArea::vertical().id_salt("yt_fields").max_height(height - 40.0).auto_shrink([false, false]).show(ui, |ui| {
                    let kind = self.doc.kind(i);
                    ui.label(egui::RichText::new(format!("{}  ({})", self.doc.name(i), kind.label())).strong().size(16.0));
                    struct_ui(ui, &self.doc, &mut self.buffers, archetype_path(i), &mut acts, 0);
                });
                self.apply(acts);
            }
        }
    }

    /// The archetype as XML text you can edit. It is checked as you type (syntax, with line and column), and Apply turns the text back
    /// into the archetype's values, so a text that does not parse never reaches the file.
    #[allow(deprecated)] // Id::new keys the editor by the window number
    fn xml_tab(&mut self, ui: &mut egui::Ui, i: usize, height: f32) {
        let key = (self.doc.undo_steps(), i, self.doc.len());
        if self.xml_edit.as_ref().map(|x| x.key) != Some(key) {
            let t = self.doc.to_xml(Some(i));
            self.xml_edit = Some(XmlEdit { key, original: t.clone(), report: metaxml::check(&t), checked: t.clone(), text: t, jump: None });
        }
        let Some(x) = self.xml_edit.as_mut() else { return };
        if x.checked != x.text {
            x.report = metaxml::check(&x.text);
            x.checked = x.text.clone();
        }
        let changed = x.text != x.original;
        let errors = x.report.errors();
        let mut apply = false;
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(changed && errors == 0, egui::Button::new("Apply")).on_hover_text(if errors > 0 { "fix the errors first" } else { "turn this text back into the archetype (one undo step)" }).clicked() {
                apply = true;
            }
            if ui.add_enabled(changed, egui::Button::new("Revert")).clicked() {
                x.text = x.original.clone();
            }
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(x.text.clone());
            }
            if errors > 0 {
                ui.label(egui::RichText::new(format!("{errors} error(s)")).color(crate::theme::err()));
            } else if x.report.diagnostics.iter().any(|d| d.severity == Severity::Warning) {
                ui.label(egui::RichText::new("the XML is well formed (with warnings)").color(crate::theme::warn()));
            } else {
                ui.label(egui::RichText::new("the XML is well formed").color(crate::theme::ok()));
            }
            if changed {
                ui.label(egui::RichText::new("not applied yet").color(crate::theme::warn()));
            }
        });
        let list_h = if x.report.diagnostics.is_empty() { 0.0 } else { 96.0 };
        let edit_h = (height - 70.0 - list_h).max(120.0);
        let findings: Vec<(usize, usize, Severity)> = x.report.diagnostics.iter().filter(|d| self.show_notes || d.severity != Severity::Info).map(|d| (char_index(&x.text, d.line, d.col), d.len, d.severity)).collect();
        let edit_id = egui::Id::new(("yt_xml_edit", self.id));
        let jump = x.jump.take();
        egui::ScrollArea::both().id_salt("yt_xml").max_height(edit_h).auto_shrink([false, false]).show(ui, |ui| {
            let out = egui::TextEdit::multiline(&mut x.text).id(edit_id).font(egui::TextStyle::Monospace).code_editor().desired_width(f32::INFINITY).lock_focus(true).show(ui);
            let painter = ui.painter_at(ui.clip_rect());
            for (start, len, sev) in &findings {
                let a = out.galley.pos_from_cursor(egui::text::CCursor::new(*start));
                let b = out.galley.pos_from_cursor(egui::text::CCursor::new(*start + (*len).max(1)));
                let x1 = if (b.min.y - a.min.y).abs() < 1.0 { b.min.x } else { a.min.x + (*len as f32).min(40.0) * 7.0 };
                squiggle(&painter, out.galley_pos + a.left_bottom().to_vec2(), (x1 - a.min.x).max(6.0), if *sev == Severity::Error { crate::theme::err() } else if *sev == Severity::Warning { crate::theme::warn() } else { crate::theme::info() });
            }
            if let Some(idx) = jump {
                let r = out.galley.pos_from_cursor(egui::text::CCursor::new(idx));
                ui.scroll_to_rect(r.translate(out.galley_pos.to_vec2()).expand(40.0), Some(egui::Align::Center));
            }
        });
        if let Some(idx) = jump {
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(idx))));
                st.store(ui.ctx(), edit_id);
            }
            ui.ctx().memory_mut(|m| m.request_focus(edit_id));
        }
        let mut go = None;
        if !x.report.diagnostics.is_empty() {
            egui::ScrollArea::vertical().id_salt("yt_xml_diag").max_height(list_h - 6.0).auto_shrink([false, true]).show(ui, |ui| {
                for d in x.report.diagnostics.iter().filter(|d| self.show_notes || d.severity != Severity::Info) {
                    ui.horizontal_wrapped(|ui| {
                        let col = match d.severity {
                            Severity::Error => crate::theme::err(),
                            Severity::Warning => crate::theme::warn(),
                            Severity::Info => crate::theme::info(),
                        };
                        if ui.link(egui::RichText::new(format!("line {}, column {}", d.line, d.col)).monospace().color(col)).clicked() {
                            go = Some(char_index(&x.text, d.line, d.col));
                        }
                        ui.label(&d.message);
                        if !d.fix.is_empty() {
                            ui.weak(format!("How to fix: {}", d.fix));
                        }
                    });
                }
            });
        }
        if go.is_some() {
            x.jump = go;
        }
        if apply {
            let text = x.text.clone();
            match self.doc.replace_archetype_from_xml(i, &text) {
                Ok(()) => {
                    self.message = Some(Ok("Applied the XML to the archetype.".into()));
                    self.buffers.clear();
                    self.xml_edit = None;
                }
                Err(e) => self.message = Some(Err(format!("{e:#}"))),
            }
        }
    }

    fn apply(&mut self, acts: Vec<Act>) {
        for a in acts {
            let r = match a {
                Act::Text(path, kind, text) => self.doc.set_text(&path, &kind, &text),
                Act::Value(path, v, label) => {
                    let key = format!("{path:?}");
                    self.doc.run(Command::Set(path, v), &label, Some(key))
                }
                Act::Insert(path, at, v) => self.doc.run(Command::Insert(path, at, v), "Add item", None),
                Act::Remove(path, at) => self.doc.run(Command::Remove(path, at), "Remove item", None),
            };
            if let Err(e) = r {
                self.message = Some(Err(format!("{e:#}")));
                self.buffers.clear();
            } else {
                self.message = None;
            }
        }
    }

    fn issues_panel(&mut self, ui: &mut egui::Ui) {
        let shown: Vec<&Issue> = self.issues.iter().filter(|i| self.show_notes || i.sev != Sev::Info).collect();
        let (e, w) = (shown.iter().filter(|i| i.sev == Sev::Error).count(), shown.iter().filter(|i| i.sev == Sev::Warning).count());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Checks").strong());
            if e == 0 && w == 0 {
                ui.label(egui::RichText::new("no problems").color(crate::theme::ok()));
            } else {
                ui.label(egui::RichText::new(format!("{e} error(s), {w} warning(s)")).color(if e > 0 { crate::theme::err() } else { crate::theme::warn() }));
            }
        });
        let mut jump = None;
        egui::ScrollArea::vertical().id_salt("yt_issues").max_height(100.0).auto_shrink([false, true]).show(ui, |ui| {
            for i in shown {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(match i.sev {
                        Sev::Error => "ERROR",
                        Sev::Warning => "WARN ",
                        Sev::Info => "NOTE ",
                    }).monospace().color(match i.sev {
                        Sev::Error => crate::theme::err(),
                        Sev::Warning => crate::theme::warn(),
                        Sev::Info => crate::theme::info(),
                    }));
                    if let Some(a) = i.archetype {
                        if ui.link(self.doc.name(a)).clicked() {
                            jump = Some(a);
                        }
                    }
                    ui.label(&i.message);
                    ui.weak(format!("How to fix: {}", i.fix));
                });
            }
        });
        if let Some(a) = jump {
            self.selected = Some(a);
            self.tab = Tab::Fields;
            self.buffers.clear();
        }
    }
}

fn summary(doc: &YtypDocument, v: &MetaValue) -> String {
    if let Some(s) = v.as_struct() {
        for n in ["name", "archetypeName", "pedXml_audioID"] {
            if let Some(x) = s.field(n) {
                let t = match x {
                    MetaValue::Hash(h) => doc.names.resolve(*h).to_string(),
                    other => schema_view::to_text(other),
                };
                if !t.is_empty() {
                    return format!("{} ({})", t, schema_view::name_of(s.type_hash));
                }
            }
        }
        return schema_view::name_of(s.type_hash);
    }
    schema_view::to_text(v)
}

fn key_of(path: &[Step]) -> String {
    format!("{path:?}")
}

/// The members of the structure at `path`, each with the widget its kind needs. Members the file lacks show as empty and are created
/// when edited; members the table does not know are shown read-only (and kept).
fn struct_ui(ui: &mut egui::Ui, doc: &YtypDocument, buffers: &mut HashMap<String, String>, path: VPath, acts: &mut Vec<Act>, depth: usize) {
    let Some(MetaValue::Struct(s)) = doc.get(&path) else { return };
    let descs = schema_view::describe(s.type_hash).unwrap_or_default();
    egui::Grid::new(("yt_grid", key_of(&path))).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
        // scalar rows go in the grid; structures and lists get their own collapsing sections after it
        for d in descs.iter().filter(|d| !matches!(d.kind, FieldKind::Struct(..) | FieldKind::Array(_))) {
            let mut p = path.clone();
            p.push(Step::Field(d.hash));
            ui.label(egui::RichText::new(&d.name).color(crate::theme::text()));
            scalar_ui(ui, doc, buffers, &p, d, acts);
            ui.end_row();
        }
        for (h, v) in &s.fields {
            if !descs.iter().any(|d| d.hash == *h) {
                ui.label(egui::RichText::new(schema_view::name_of(*h)).color(crate::theme::dim()));
                ui.label(egui::RichText::new(format!("{} (kept as is)", schema_view::to_text(v))).color(crate::theme::dim()));
                ui.end_row();
            }
        }
    });
    for d in descs.iter().filter(|d| matches!(d.kind, FieldKind::Struct(..) | FieldKind::Array(_))) {
        let mut p = path.clone();
        p.push(Step::Field(d.hash));
        nested_ui(ui, doc, buffers, &p, d, acts, depth);
    }
}

fn scalar_ui(ui: &mut egui::Ui, doc: &YtypDocument, buffers: &mut HashMap<String, String>, p: &VPath, d: &FieldDesc, acts: &mut Vec<Act>) {
    let value = doc.get(p);
    let text_now = value.map(schema_view::to_text).unwrap_or_else(|| schema_view::to_text(&schema_view::default_value(&d.kind)));
    match &d.kind {
        FieldKind::Bool => {
            let mut b = matches!(value, Some(MetaValue::Bool(true)));
            if ui.checkbox(&mut b, "").changed() {
                acts.push(Act::Value(p.clone(), MetaValue::Bool(b), format!("Edit {}", d.name)));
            }
        }
        FieldKind::Enum(h) => {
            let members = schema_view::enum_members(*h);
            egui::ComboBox::from_id_salt(("yt_enum", key_of(p))).selected_text(text_now.clone()).width(240.0).show_ui(ui, |ui| {
                for (nh, _) in &members {
                    let n = schema_view::name_of(*nh);
                    if ui.selectable_label(n == text_now, &n).clicked() {
                        acts.push(Act::Text(p.clone(), d.kind.clone(), n));
                    }
                }
            });
        }
        FieldKind::Flags(h) => {
            let bits = match value {
                Some(MetaValue::Flags { bits, .. }) => *bits,
                _ => 0,
            };
            let members = schema_view::enum_members(*h);
            egui::CollapsingHeader::new(if text_now.is_empty() { "(none)".to_string() } else { text_now.clone() }).id_salt(("yt_flags", key_of(p))).show(ui, |ui| {
                for (nh, v) in &members {
                    let mask = *v as u32;
                    if mask == 0 {
                        continue;
                    }
                    let mut on = bits & mask == mask;
                    if ui.checkbox(&mut on, schema_view::name_of(*nh)).changed() {
                        let new = if on { bits | mask } else { bits & !mask };
                        let names = members.iter().filter(|(_, v)| *v as u32 & new == *v as u32 && *v != 0).map(|(n, _)| *n).collect();
                        acts.push(Act::Value(p.clone(), MetaValue::Flags { enum_hash: *h, bits: new, names }, format!("Edit {}", d.name)));
                    }
                }
                ui.weak(format!("raw value {bits}"));
            });
        }
        FieldKind::Int(..) | FieldKind::Float | FieldKind::Vector(_) | FieldKind::Text | FieldKind::Hash => {
            let key = key_of(p);
            let buf = buffers.entry(key.clone()).or_insert_with(|| text_now.clone());
            let invalid = schema_view::parse_text(&d.kind, buf, value).is_err();
            let r = ui.add(egui::TextEdit::singleline(buf).desired_width(240.0).text_color(if invalid { crate::theme::err() } else { ui.visuals().text_color() }));
            if r.lost_focus() || (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                if *buf != text_now {
                    acts.push(Act::Text(p.clone(), d.kind.clone(), buf.clone()));
                }
                buffers.remove(&key);
            } else if !r.has_focus() && *buf != text_now {
                // changed elsewhere (undo, another edit): follow it
                buffers.remove(&key);
            }
            if invalid {
                r.on_hover_text(schema_view::parse_text(&d.kind, &text_now, value).map(|_| "this is not a valid value".to_string()).unwrap_or_default());
            }
        }
        FieldKind::Other => {
            ui.label(egui::RichText::new(text_now).color(crate::theme::dim()));
        }
        FieldKind::Struct(..) | FieldKind::Array(_) => {}
    }
}

fn nested_ui(ui: &mut egui::Ui, doc: &YtypDocument, buffers: &mut HashMap<String, String>, p: &VPath, d: &FieldDesc, acts: &mut Vec<Act>, depth: usize) {
    match &d.kind {
        FieldKind::Struct(h, _) => {
            egui::CollapsingHeader::new(&d.name).id_salt(("yt_struct", key_of(p))).show(ui, |ui| match doc.get(p) {
                Some(MetaValue::Struct(_)) => struct_ui(ui, doc, buffers, p.clone(), acts, depth + 1),
                _ => {
                    ui.weak("(empty)");
                    if ui.small_button("Create").clicked() {
                        acts.push(Act::Value(p.clone(), schema_view::default_value(&FieldKind::Struct(*h, false)), format!("Add {}", d.name)));
                    }
                }
            });
        }
        FieldKind::Array(elem) => {
            let items: &[MetaValue] = doc.get(p).map(|v| v.items()).unwrap_or(&[]);
            egui::CollapsingHeader::new(format!("{}  ({})", d.name, items.len())).id_salt(("yt_array", key_of(p))).show(ui, |ui| {
                for (n, item) in items.iter().enumerate() {
                    let mut ip = p.clone();
                    ip.push(Step::Index(n));
                    ui.horizontal(|ui| {
                        egui::CollapsingHeader::new(format!("{n}  {}", summary(doc, item))).id_salt(("yt_item", key_of(&ip))).show(ui, |ui| match (&**elem, item) {
                            (_, MetaValue::Struct(_)) => struct_ui(ui, doc, buffers, ip.clone(), acts, depth + 1),
                            _ => {
                                ui.label(schema_view::to_text(item));
                            }
                        });
                        if ui.small_button("x").on_hover_text("Remove this item").clicked() {
                            acts.push(Act::Remove(p.clone(), n));
                        }
                    });
                }
                match &**elem {
                    FieldKind::Struct(h, ptr) if *h != 0 => {
                        if ui.small_button(format!("Add {}", schema_view::name_of(*h))).clicked() {
                            let mut v = schema_view::default_value(&FieldKind::Struct(*h, *ptr));
                            // a new item starts with every member the table lists, empty
                            if let (MetaValue::Struct(s), Some(descs)) = (&mut v, schema_view::describe(*h)) {
                                s.fields = descs.iter().map(|f| (f.hash, schema_view::default_value(&f.kind))).collect();
                            }
                            acts.push(Act::Insert(p.clone(), items.len(), v));
                        }
                    }
                    FieldKind::Struct(..) => {
                        let prefixes: &[&str] = match d.name.as_str() {
                            "extensions" => &["CExtensionDef"],
                            "entities" => &["CEntityDef", "CMloInstanceDef"],
                            _ => &[],
                        };
                        if prefixes.is_empty() {
                            ui.weak("items of this list can be different structures: edit them in the XML tab");
                        } else {
                            ui.menu_button("Add...", |ui| {
                                for (name, h) in prefixes.iter().flat_map(|p| schema_view::struct_names_with_prefix(p)) {
                                    if ui.button(&name).clicked() {
                                        acts.push(Act::Insert(p.clone(), items.len(), schema_view::new_struct(h)));
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }
                    _ => {}
                }
            });
        }
        _ => {}
    }
}

/// the index in characters of a 1-based line and column
fn char_index(text: &str, line: usize, col: usize) -> usize {
    let mut idx = 0;
    for (n, l) in text.split('\n').enumerate() {
        if n + 1 == line {
            return idx + col.saturating_sub(1).min(l.chars().count());
        }
        idx += l.chars().count() + 1;
    }
    text.chars().count()
}

/// a wavy line starting at `from`, `width` long
fn squiggle(painter: &egui::Painter, from: egui::Pos2, width: f32, col: Color32) {
    let step = 3.0;
    let n = (width / step).ceil() as usize + 1;
    let pts: Vec<egui::Pos2> = (0..n).map(|i| egui::Pos2::new(from.x + i as f32 * step, from.y + if i % 2 == 0 { 0.0 } else { 2.0 })).collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.3, col)));
}
