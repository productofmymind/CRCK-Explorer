//! Batch rename: pick files (add folders, drop them, Ctrl / Shift click to select, filter by extension), build a list of steps
//! (numbers, texture letters, drawable numbers, find and replace, case, patterns, or a preset), see every old and new name with the
//! conflicts marked, apply, undo. The engine and its safety rules are `rage_toolkit_core::rename`.
use super::*;
use rage_toolkit_core::rename::{self, Case, Op, Plan, SeqMode, Sort, Status, Token, UndoLog};
use std::collections::BTreeSet;

/// One step as the form holds it (text boxes stay text until the step is turned into an `Op`).
#[derive(Clone, PartialEq)]
enum Kind {
    Sequential { start: String, digits: String, prefix: String, suffix: String, append: bool, per_folder: bool },
    Letters { token: String },
    Renumber { token: String, start: String },
    Replace { find: String, with: String, regex: bool, case: bool },
    AddPrefix(String),
    AddSuffix(String),
    RemovePrefix(String),
    RemoveSuffix(String),
    Case(Case),
    Pattern(String),
}

#[derive(Clone, PartialEq)]
struct Step {
    on: bool,
    kind: Kind,
}

const ADDABLE: [(&str, &str); 10] = [
    ("Number the files", "001, 002, 003 ... in the chosen order"),
    ("Texture letters a, b, c", "GTA V: variations that differ in one letter get consecutive letters"),
    ("Drawable numbers", "GTA V: close the gaps in 000, 003, 007 ..."),
    ("Find and replace", "text or a regular expression"),
    ("Add a prefix", ""),
    ("Add a suffix", ""),
    ("Remove a prefix", ""),
    ("Remove a suffix", ""),
    ("Change the case", "lower, UPPER, Title"),
    ("Pattern", "your own template: {name} {ext} {parent} {n} {n:3}"),
];

fn new_step(i: usize) -> Kind {
    match i {
        0 => Kind::Sequential { start: "1".into(), digits: "3".into(), prefix: String::new(), suffix: String::new(), append: false, per_folder: false },
        1 => Kind::Letters { token: String::new() },
        2 => Kind::Renumber { token: String::new(), start: "0".into() },
        3 => Kind::Replace { find: String::new(), with: String::new(), regex: false, case: true },
        4 => Kind::AddPrefix(String::new()),
        5 => Kind::AddSuffix(String::new()),
        6 => Kind::RemovePrefix(String::new()),
        7 => Kind::RemoveSuffix(String::new()),
        8 => Kind::Case(Case::Lower),
        _ => Kind::Pattern("{name}".into()),
    }
}

fn token_of(s: &str) -> Result<Token, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        Ok(Token::Auto)
    } else {
        s.parse::<i32>().map(Token::Index).map_err(|_| format!("'{s}' is not a part number (leave it empty for automatic, 0 = first part, -1 = last)"))
    }
}

fn number<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, String> {
    s.trim().parse().map_err(|_| format!("{what} '{s}' is not a number"))
}

impl Kind {
    fn to_op(&self) -> Result<Op, String> {
        Ok(match self {
            Kind::Sequential { start, digits, prefix, suffix, append, per_folder } => Op::Sequential { start: number(start, "the start")?, digits: number(digits, "the digit count")?, prefix: prefix.clone(), suffix: suffix.clone(), mode: if *append { SeqMode::Append } else { SeqMode::Replace }, restart_per_folder: *per_folder },
            Kind::Letters { token } => Op::Letters { token: token_of(token)? },
            Kind::Renumber { token, start } => Op::Renumber { token: token_of(token)?, start: number(start, "the start")? },
            Kind::Replace { find, with, regex, case } => Op::Replace { find: find.clone(), with: with.clone(), regex: *regex, case_sensitive: *case },
            Kind::AddPrefix(s) => Op::AddPrefix(s.clone()),
            Kind::AddSuffix(s) => Op::AddSuffix(s.clone()),
            Kind::RemovePrefix(s) => Op::RemovePrefix(s.clone()),
            Kind::RemoveSuffix(s) => Op::RemoveSuffix(s.clone()),
            Kind::Case(c) => Op::Case(*c),
            Kind::Pattern(p) => Op::Pattern(p.clone()),
        })
    }

    fn from_op(op: &Op) -> Kind {
        let tok = |t: &Token| match t {
            Token::Auto => String::new(),
            Token::Index(i) => i.to_string(),
        };
        match op {
            Op::Sequential { start, digits, prefix, suffix, mode, restart_per_folder } => Kind::Sequential { start: start.to_string(), digits: digits.to_string(), prefix: prefix.clone(), suffix: suffix.clone(), append: *mode == SeqMode::Append, per_folder: *restart_per_folder },
            Op::Letters { token } => Kind::Letters { token: tok(token) },
            Op::Renumber { token, start } => Kind::Renumber { token: tok(token), start: start.to_string() },
            Op::Replace { find, with, regex, case_sensitive } => Kind::Replace { find: find.clone(), with: with.clone(), regex: *regex, case: *case_sensitive },
            Op::AddPrefix(s) => Kind::AddPrefix(s.clone()),
            Op::AddSuffix(s) => Kind::AddSuffix(s.clone()),
            Op::RemovePrefix(s) => Kind::RemovePrefix(s.clone()),
            Op::RemoveSuffix(s) => Kind::RemoveSuffix(s.clone()),
            Op::Case(c) => Kind::Case(*c),
            Op::Pattern(p) => Kind::Pattern(p.clone()),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Kind::Sequential { .. } => "Number the files",
            Kind::Letters { .. } => "Texture letters a, b, c ...",
            Kind::Renumber { .. } => "Drawable numbers",
            Kind::Replace { .. } => "Find and replace",
            Kind::AddPrefix(_) => "Add a prefix",
            Kind::AddSuffix(_) => "Add a suffix",
            Kind::RemovePrefix(_) => "Remove a prefix",
            Kind::RemoveSuffix(_) => "Remove a suffix",
            Kind::Case(_) => "Change the case",
            Kind::Pattern(_) => "Pattern",
        }
    }
}

/// `ytd, .ydd` -> ["ytd", "ydd"]
fn exts_of(filter: &str) -> Vec<String> {
    filter.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).map(|e| e.trim().trim_start_matches('.').trim_start_matches("*.").to_lowercase()).filter(|e| !e.is_empty()).collect()
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

pub struct State {
    files: Vec<PathBuf>,
    /// indices into `files`
    selected: BTreeSet<usize>,
    /// where a Shift click starts, as an index into `files`
    anchor: Option<usize>,
    ext_filter: String,
    name_filter: String,
    deep: bool,
    steps: Vec<Step>,
    sort: Sort,
    reverse: bool,
    plan: Plan,
    dirty: bool,
    undo: Vec<UndoLog>,
    message: Option<Result<String, String>>,
}

impl Default for State {
    fn default() -> Self {
        // the last batch of an earlier run can still be undone
        let undo = std::fs::read_to_string(rename::log_path()).map(|t| rename::log_from_text(&t)).ok().filter(|l| !l.moves.is_empty()).into_iter().collect();
        Self { files: vec![], selected: BTreeSet::new(), anchor: None, ext_filter: String::new(), name_filter: String::new(), deep: false, steps: vec![], sort: Sort::Natural, reverse: false, plan: Plan::default(), dirty: true, undo, message: None }
    }
}

impl State {
    /// Files and folders dropped on the window while this page is open.
    pub fn add_dropped(&mut self, paths: &[PathBuf]) -> bool {
        let mut any = false;
        for p in paths {
            let found = if p.is_dir() { rename::files_in(p, self.deep) } else if p.is_file() { vec![p.clone()] } else { vec![] };
            for f in found {
                if !self.files.contains(&f) {
                    self.files.push(f);
                    any = true;
                }
            }
        }
        if any {
            self.dirty = true;
            self.message = None;
        }
        any
    }

    /// `--rename-preset N` (automated screenshots): start from preset N
    pub fn use_preset(&mut self, n: usize) {
        if let Some((_, _, ops)) = rename::presets().into_iter().nth(n) {
            self.steps = ops.iter().map(|o| Step { on: true, kind: Kind::from_op(o) }).collect();
            self.dirty = true;
        }
    }

    /// indices into `files` that pass the filters
    fn visible(&self) -> Vec<usize> {
        let exts = exts_of(&self.ext_filter);
        let name = self.name_filter.trim().to_lowercase();
        (0..self.files.len())
            .filter(|&i| {
                let f = &self.files[i];
                (exts.is_empty() || f.extension().map(|e| exts.contains(&e.to_string_lossy().to_lowercase())).unwrap_or(false)) && (name.is_empty() || name_of(f).to_lowercase().contains(&name))
            })
            .collect()
    }

    /// the files to rename: the selected ones that are visible, or every visible one when nothing is selected
    fn sources(&self) -> Vec<PathBuf> {
        let vis = self.visible();
        let chosen: Vec<usize> = vis.iter().copied().filter(|i| self.selected.contains(i)).collect();
        let use_ = if chosen.is_empty() { vis } else { chosen };
        use_.into_iter().map(|i| self.files[i].clone()).collect()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let _ = ctx;
        card(ui, "1. Files", |ui| {
            self.files_section(ui);
            if self.files.is_empty() {
                ui.add_space(6.0);
                drop_zone(ui, "Drop files and folders here");
            }
        });
        ui.add_space(10.0);
        card(ui, "2. What to do", |ui| self.steps_section(ui));
        ui.add_space(10.0);
        card(ui, "3. Preview", |ui| self.preview_section(ui));
    }

    fn files_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add folder...").clicked() {
                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                    self.add_dropped(&[d]);
                }
            }
            if ui.button("Add files...").clicked() {
                if let Some(fs) = rfd::FileDialog::new().pick_files() {
                    self.add_dropped(&fs);
                }
            }
            ui.checkbox(&mut self.deep, "include subfolders").on_hover_text("when adding a folder");
            if ui.add_enabled(!self.files.is_empty(), egui::Button::new("Clear list")).clicked() {
                self.files.clear();
                self.selected.clear();
                self.anchor = None;
                self.dirty = true;
            }
            ui.weak("or drop files and folders on the window");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Only extensions:");
            if ui.add(egui::TextEdit::singleline(&mut self.ext_filter).desired_width(120.0).hint_text("ytd, ydd")).changed() {
                self.dirty = true;
            }
            // one click per extension that is in the list
            let mut present: Vec<String> = self.files.iter().filter_map(|f| f.extension().map(|e| e.to_string_lossy().to_lowercase())).collect();
            present.sort();
            present.dedup();
            let current = exts_of(&self.ext_filter);
            for e in present.iter().take(12) {
                if ui.selectable_label(current.contains(e), format!(".{e}")).clicked() {
                    let mut now = current.clone();
                    if let Some(p) = now.iter().position(|x| x == e) {
                        now.remove(p);
                    } else {
                        now.push(e.clone());
                    }
                    self.ext_filter = now.join(", ");
                    self.dirty = true;
                }
            }
            ui.label("Name contains:");
            if ui.add(egui::TextEdit::singleline(&mut self.name_filter).desired_width(120.0)).changed() {
                self.dirty = true;
            }
        });
        let vis = self.visible();
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Select all shown").clicked() {
                self.selected.extend(vis.iter().copied());
                self.dirty = true;
            }
            if ui.small_button("Select none").clicked() {
                self.selected.clear();
                self.dirty = true;
            }
            let any_selected = vis.iter().any(|i| self.selected.contains(i));
            if ui.add_enabled(any_selected, egui::Button::new("Remove selected").small()).on_hover_text("Take the selected files out of the list (the files themselves are not touched). Delete key does the same.").clicked() {
                self.remove_selected();
            }
            if ui.small_button("Invert").clicked() {
                for &i in &vis {
                    if !self.selected.remove(&i) {
                        self.selected.insert(i);
                    }
                }
                self.dirty = true;
            }
            let sel = vis.iter().filter(|i| self.selected.contains(i)).count();
            ui.weak(format!("{} file(s), {} shown, {} selected{}", self.files.len(), vis.len(), sel, if sel == 0 { " (nothing selected: all shown files are renamed)" } else { "" }));
            ui.weak("Click, Ctrl+click, Shift+click, Ctrl+Shift+click");
        });
        let modifiers = ui.input(|i| i.modifiers);
        if !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Delete)) && vis.iter().any(|i| self.selected.contains(i)) {
            self.remove_selected();
            return;
        }
        let row_h = 20.0;
        let mut clicked: Option<usize> = None;
        let height = (vis.len() as f32 * row_h).clamp(60.0, 180.0);
        egui::ScrollArea::vertical().id_salt("rn_files").max_height(height).auto_shrink([false, true]).show_rows(ui, row_h, vis.len(), |ui, range| {
            for pos in range {
                let i = vis[pos];
                let f = &self.files[i];
                let dir = f.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                // a row painted by hand: name on the left, folder on the right
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                let on = self.selected.contains(&i);
                if on {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                } else if r.hovered() {
                    ui.painter().rect_filled(rect, 3.0, crate::theme::surface_hi());
                }
                ui.painter().text(egui::pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, name_of(f), egui::FontId::proportional(14.0), if on { crate::theme::text() } else { crate::theme::text() });
                ui.painter().text(egui::pos2(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, dir, egui::FontId::proportional(12.0), crate::theme::dim());
                let r = r.on_hover_text(f.display().to_string());
                if r.clicked() {
                    clicked = Some(pos);
                }
            }
        });
        if let Some(pos) = clicked {
            let i = vis[pos];
            let anchor_pos = self.anchor.and_then(|a| vis.iter().position(|&v| v == a));
            let range: Vec<usize> = match (modifiers.shift, anchor_pos) {
                (true, Some(a)) => vis[a.min(pos)..=a.max(pos)].to_vec(),
                _ => vec![i],
            };
            if modifiers.shift {
                if !modifiers.command {
                    self.selected.clear();
                }
                self.selected.extend(range);
            } else if modifiers.command {
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
                self.anchor = Some(i);
            } else {
                self.selected.clear();
                self.selected.insert(i);
                self.anchor = Some(i);
            }
            self.dirty = true;
        }
    }

    /// take the selected files out of the list; the files on disk are not touched
    fn remove_selected(&mut self) {
        let sel = std::mem::take(&mut self.selected);
        let mut i = 0;
        self.files.retain(|_| {
            let keep = !sel.contains(&i);
            i += 1;
            keep
        });
        self.anchor = None;
        self.dirty = true;
    }

    fn steps_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("rn_preset").selected_text("Use a preset...").width(260.0).show_ui(ui, |ui| {
                for (name, about, ops) in rename::presets() {
                    if ui.selectable_label(false, name).on_hover_text(about).clicked() {
                        self.steps = ops.iter().map(|o| Step { on: true, kind: Kind::from_op(o) }).collect();
                        self.dirty = true;
                    }
                }
            });
            ui.menu_button("Add a step", |ui| {
                for (i, (name, about)) in ADDABLE.iter().enumerate() {
                    let r = ui.button(*name);
                    if !about.is_empty() {
                        r.clone().on_hover_text(*about);
                    }
                    if r.clicked() {
                        self.steps.push(Step { on: true, kind: new_step(i) });
                        self.dirty = true;
                        ui.close();
                    }
                }
            });
            if ui.add_enabled(!self.steps.is_empty(), egui::Button::new("Remove all steps")).clicked() {
                self.steps.clear();
                self.dirty = true;
            }
            ui.label("Order the files by:");
            let label = match self.sort {
                Sort::Given => "as listed",
                Sort::Alphabetical => "A-Z",
                Sort::Natural => "natural (2 before 10)",
                Sort::Numeric => "first number in the name",
                Sort::Extension => "extension",
            };
            egui::ComboBox::from_id_salt("rn_sort").selected_text(label).width(190.0).show_ui(ui, |ui| {
                for (s, l) in [(Sort::Given, "as listed"), (Sort::Alphabetical, "A-Z"), (Sort::Natural, "natural (2 before 10)"), (Sort::Numeric, "first number in the name"), (Sort::Extension, "extension")] {
                    if ui.selectable_value(&mut self.sort, s, l).changed() {
                        self.dirty = true;
                    }
                }
            });
            if ui.checkbox(&mut self.reverse, "reversed").changed() {
                self.dirty = true;
            }
        });
        if self.steps.is_empty() {
            ui.weak("No steps yet: pick a preset, or add a step.");
        }
        let (mut remove, mut up, mut down) = (None, None, None);
        let n = self.steps.len();
        for (i, step) in self.steps.iter_mut().enumerate() {
            let before = step.clone();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut step.on, "");
                    ui.label(egui::RichText::new(format!("{}. {}", i + 1, step.kind.title())).strong());
                    if ui.add_enabled(i > 0, egui::Button::new("^").small()).on_hover_text("Earlier").clicked() {
                        up = Some(i);
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("v").small()).on_hover_text("Later").clicked() {
                        down = Some(i);
                    }
                    if ui.small_button("x").on_hover_text("Remove this step").clicked() {
                        remove = Some(i);
                    }
                });
                step_fields(ui, &mut step.kind);
            });
            if *step != before {
                self.dirty = true;
            }
        }
        if let Some(i) = remove {
            self.steps.remove(i);
        }
        if let Some(i) = up {
            self.steps.swap(i, i - 1);
        }
        if let Some(i) = down {
            self.steps.swap(i, i + 1);
        }
        if remove.is_some() || up.is_some() || down.is_some() {
            self.dirty = true;
        }
    }

    fn preview_section(&mut self, ui: &mut egui::Ui) {
        if self.dirty {
            let mut ops = vec![];
            let mut bad = vec![];
            for s in self.steps.iter().filter(|s| s.on) {
                match s.kind.to_op() {
                    Ok(op) => ops.push(op),
                    Err(e) => bad.push(e),
                }
            }
            self.plan = rename::plan(&self.sources(), self.sort, self.reverse, &ops);
            self.plan.warnings.extend(bad);
            for s in self.steps.iter().filter(|s| s.on) {
                if let Kind::Replace { find, regex: true, .. } = &s.kind {
                    if let Some(e) = rename::regex_error(find) {
                        if !self.plan.warnings.iter().any(|w| w.contains("regular expression")) {
                            self.plan.warnings.push(format!("the regular expression {find:?} is not valid: {e}"));
                        }
                    }
                }
            }
            self.dirty = false;
        }
        for w in &self.plan.warnings {
            ui.label(egui::RichText::new(w).color(crate::theme::warn()));
        }
        let (ready, problems) = (self.plan.ready(), self.plan.problems());
        ui.horizontal_wrapped(|ui| {
            let can = self.plan.can_apply();
            if ui.add_enabled(can, big_button(&format!("Rename {ready} file(s)")).fill(if can { crate::theme::accent().gamma_multiply(0.5) } else { crate::theme::surface_hi() })).on_disabled_hover_text(if problems > 0 { "fix the conflicts first" } else { "nothing to rename" }).clicked() {
                self.apply();
            }
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo the last rename")).clicked() {
                self.undo_last();
            }
            if problems > 0 {
                ui.label(egui::RichText::new(format!("{problems} name(s) in the way: nothing is renamed until they are fixed")).color(crate::theme::err()));
            } else if ready == 0 && !self.plan.rows.is_empty() {
                ui.weak("No name changes.");
            }
        });
        if let Some(m) = &self.message {
            match m {
                Ok(t) => ui.label(egui::RichText::new(t).color(crate::theme::ok())),
                Err(t) => ui.label(egui::RichText::new(t).color(crate::theme::err())),
            };
        }
        let rows = &self.plan.rows;
        let row_h = 20.0;
        let width = ui.available_width();
        let col = ((width - 150.0) / 2.0).max(150.0);
        egui::ScrollArea::vertical().id_salt("rn_preview").max_height(280.0).auto_shrink([false, true]).show_rows(ui, row_h, rows.len(), |ui, range| {
            for r in &rows[range] {
                ui.horizontal(|ui| {
                    cell(ui, col, row_h, egui::RichText::new(name_of(&r.from)).color(if r.status == Status::Unchanged { crate::theme::dim() } else { crate::theme::text() }));
                    let (arrow, text, colour) = match &r.status {
                        Status::Unchanged => ("  ", "(same)".to_string(), crate::theme::dim()),
                        Status::Ready => ("->", name_of(&r.to), crate::theme::ok()),
                        Status::Conflict(why) => ("!!", format!("{}   [{why}]", name_of(&r.to)), crate::theme::err()),
                        Status::Invalid(why) => ("!!", format!("{}   [{why}]", name_of(&r.to)), crate::theme::err()),
                    };
                    ui.label(egui::RichText::new(arrow).monospace().color(colour));
                    cell(ui, col, row_h, egui::RichText::new(text).color(colour));
                });
            }
        });
        if rows.is_empty() {
            ui.weak("Add files above to see the new names here.");
        }
    }

    fn apply(&mut self) {
        match rename::apply(&self.plan) {
            Ok(log) => {
                let n = log.moves.len();
                self.replace_paths(&log.moves, false);
                let _ = std::fs::write(rename::log_path(), rename::log_to_text(&log));
                self.undo.push(log);
                self.message = Some(Ok(format!("Renamed {n} file(s). Undo puts the old names back.")));
            }
            Err(e) => self.message = Some(Err(format!("{e:#}"))),
        }
        self.dirty = true;
    }

    fn undo_last(&mut self) {
        let Some(log) = self.undo.pop() else { return };
        match rename::undo(&log) {
            Ok(()) => {
                self.replace_paths(&log.moves, true);
                if self.undo.is_empty() {
                    let _ = std::fs::remove_file(rename::log_path());
                }
                self.message = Some(Ok(format!("Undone: {} file(s) have their old names back.", log.moves.len())));
            }
            Err(e) => {
                self.undo.push(log);
                self.message = Some(Err(format!("{e:#}")));
            }
        }
        self.dirty = true;
    }

    /// the list shows the new names after an apply (or the old ones after an undo)
    fn replace_paths(&mut self, moves: &[(PathBuf, PathBuf)], backwards: bool) {
        for (orig, now) in moves {
            let (from, to) = if backwards { (now, orig) } else { (orig, now) };
            if let Some(f) = self.files.iter_mut().find(|f| *f == from) {
                *f = to.clone();
            }
        }
    }
}

/// a left-aligned, truncated text cell of a fixed width
fn cell(ui: &mut egui::Ui, w: f32, h: f32, text: egui::RichText) {
    ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(egui::vec2(w, h));
        ui.add(egui::Label::new(text).truncate());
    });
}

/// the form fields of one step
fn step_fields(ui: &mut egui::Ui, kind: &mut Kind) {
    let text = |ui: &mut egui::Ui, label: &str, value: &mut String, width: f32, hint: &str| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).desired_width(width).hint_text(hint));
    };
    match kind {
        Kind::Sequential { start, digits, prefix, suffix, append, per_folder } => {
            ui.weak("Replaces the whole name with a number (001.ytd). To keep GTA names like hand_diff_001_a_uni, use the steps \"Drawable numbers\" and \"Texture letters\" instead.");
            ui.horizontal_wrapped(|ui| {
                text(ui, "start at", start, 50.0, "1");
                text(ui, "digits", digits, 36.0, "3");
                text(ui, "prefix", prefix, 90.0, "");
                text(ui, "suffix", suffix, 90.0, "");
                ui.checkbox(append, "after the old name");
                ui.checkbox(per_folder, "restart in each folder");
            });
        }
        Kind::Letters { token } => {
            ui.horizontal_wrapped(|ui| {
                text(ui, "letter part", token, 50.0, "auto");
                ui.weak("jbib_diff_001_a_uni: the single letter between underscores is found by itself; or give its position (0 = first part, -1 = last)");
            });
        }
        Kind::Renumber { token, start } => {
            ui.horizontal_wrapped(|ui| {
                text(ui, "number part", token, 50.0, "auto");
                text(ui, "start at", start, 50.0, "0");
                ui.weak("each component (jbib, uppr...) is renumbered on its own; drawables and their textures stay matched");
            });
        }
        Kind::Replace { find, with, regex, case } => {
            ui.horizontal_wrapped(|ui| {
                text(ui, "find", find, 160.0, "");
                text(ui, "replace with", with, 160.0, "");
                ui.checkbox(regex, "regular expression");
                ui.checkbox(case, "match case");
            });
            if *regex {
                if let Some(e) = rename::regex_error(find) {
                    ui.label(egui::RichText::new(e).color(crate::theme::err()).small());
                } else {
                    ui.weak("groups: $1, ${2}; for example  ^(\\w+)_hi$  ->  $1");
                }
            }
        }
        Kind::AddPrefix(s) | Kind::AddSuffix(s) | Kind::RemovePrefix(s) | Kind::RemoveSuffix(s) => {
            ui.horizontal(|ui| {
                text(ui, "text", s, 220.0, "");
            });
        }
        Kind::Case(c) => {
            ui.horizontal(|ui| {
                ui.radio_value(c, Case::Lower, "lowercase");
                ui.radio_value(c, Case::Upper, "UPPERCASE");
                ui.radio_value(c, Case::Title, "Title Case");
            });
        }
        Kind::Pattern(p) => {
            ui.horizontal_wrapped(|ui| {
                text(ui, "new name", p, 300.0, "{name}");
                ui.weak("{name} {ext} {parent} {n} {n:3}  and  {name|upper} {name|lower}");
            });
        }
    }
}
