//! The handling editor: open a handling.meta, change values with the vanilla range next to them, copy a vanilla vehicle's
//! handling, save. Edits are made to the file's text (`rage_toolkit_core::handling_edit`), so nothing else in it is reformatted.
use super::*;
use rage_toolkit_core::{
    handling::{self, Handling, Kind, Parsed, Source, Value},
    handling_edit::{self, FieldRef},
};
use std::{collections::HashMap, fs, sync::Arc};

/// The game's own handling data, to compare with and to copy from.
struct Vanilla {
    sources: Vec<Source>,
    /// lowercase handlingName -> (index in `sources`, entry); the last definition wins, like in the game
    index: HashMap<String, (usize, Handling)>,
}

fn load_vanilla() -> anyhow::Result<Vanilla> {
    let sources = handling::game_sources(None)?;
    let mut index = HashMap::new();
    for (i, s) in sources.iter().enumerate() {
        if let Ok(p) = handling::parse(&s.text) {
            for h in p.items {
                index.insert(h.name.to_lowercase(), (i, h));
            }
        }
    }
    Ok(Vanilla { sources, index })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Copy,
    Rename,
    AddVanilla,
}

const SECTIONS: [&str; 8] = ["Mass, drag and balance", "Engine and gearbox", "Brakes", "Steering", "Traction", "Suspension", "Damage, fuel and misc", "Flags"];

fn section_of(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let has = |p: &str| n.starts_with(p);
    if matches!(n.as_str(), "fmass" | "finitialdragcoeff" | "fpercentsubmerged" | "veccentreofmassoffset" | "vecinertiamultiplier" | "fdownforcemodifier") {
        SECTIONS[0]
    } else if has("fdrive") || has("ninitialdrive") || has("finitialdrive") || has("fclutch") || n == "fboostmaxspeed" || n == "frocketboostcapacity" {
        SECTIONS[1]
    } else if has("fbrake") || n == "fhandbrakeforce" {
        SECTIONS[2]
    } else if n == "fsteeringlock" {
        SECTIONS[3]
    } else if has("ftraction") || has("flowspeedtraction") || has("fcamber") {
        SECTIONS[4]
    } else if has("fsuspension") || has("fantiroll") || has("frollcentre") {
        SECTIONS[5]
    } else if has("str") || n == "aihandling" {
        SECTIONS[7]
    } else {
        SECTIONS[6]
    }
}

fn fmt_value(v: &Value) -> String {
    let n = |x: f64| format!("{x}");
    match v {
        Value::Num(x) => n(*x),
        Value::Vec3(c) => format!("{}, {}, {}", n(c[0]), n(c[1]), n(c[2])),
        Value::Text(s) => s.clone(),
        Value::Bad(s) => s.clone(),
        Value::Raw(_) => "(complex)".into(),
    }
}

/// Integer fields (gears, money): dragged in whole steps.
fn is_int(name: &str) -> bool {
    name.starts_with('n') && name.chars().nth(1).is_some_and(char::is_uppercase)
}

/// The editing widget for one value. Returns (new value if it changed, an edit has just begun: snapshot for undo).
fn edit_value(ui: &mut egui::Ui, name: &str, v: &Value) -> (Option<Value>, bool) {
    // a drag gives numbers like 0.35000000000000003: keep six decimals
    let round = |x: f64| (x * 1e6).round() / 1e6;
    match v {
        Value::Num(x) => {
            let mut f = *x;
            let mut dv = egui::DragValue::new(&mut f);
            dv = if is_int(name) { dv.fixed_decimals(0).speed(0.05) } else { dv.max_decimals(6).speed((x.abs() * 0.002).max(0.0005)) };
            let r = ui.add(dv);
            ((r.changed() && round(f) != *x).then_some(Value::Num(round(f))), r.drag_started() || r.gained_focus())
        }
        Value::Vec3(c) => {
            let (mut n, mut begin, mut changed) = (*c, false, false);
            ui.horizontal(|ui| {
                for (k, label) in ["x", "y", "z"].iter().enumerate() {
                    ui.label(egui::RichText::new(*label).small().color(crate::theme::dim()));
                    let r = ui.add(egui::DragValue::new(&mut n[k]).max_decimals(6).speed((c[k].abs() * 0.002).max(0.001)));
                    begin |= r.drag_started() || r.gained_focus();
                    changed |= r.changed();
                }
            });
            let n = n.map(round);
            ((changed && n != *c).then_some(Value::Vec3(n)), begin)
        }
        Value::Text(s) => {
            let mut t = s.clone();
            let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(120.0).font(egui::TextStyle::Monospace));
            ((r.changed() && t != *s).then_some(Value::Text(t)), r.gained_focus())
        }
        Value::Bad(raw) => {
            ui.colored_label(crate::theme::err(), format!("not a number: {raw}"));
            let mut f = 0.0;
            let r = ui.add(egui::DragValue::new(&mut f).speed(0.01)).on_hover_text("Drag or type a number to replace it");
            ((r.changed() && f != 0.0).then_some(Value::Num(f)), r.drag_started() || r.gained_focus())
        }
        Value::Raw(src) => {
            ui.label(egui::RichText::new("(complex element: edit it in a text editor)").small().color(crate::theme::dim())).on_hover_text(src.as_str());
            (None, false)
        }
    }
}

fn range_text(lo: f64, hi: f64) -> String {
    format!("{lo}..{hi}")
}

#[derive(Default)]
pub struct State {
    path: Option<PathBuf>,
    text: String,
    saved: String,
    undo: Vec<String>,
    /// the text when the current drag / typing began, for one undo step per gesture
    gesture_text: Option<String>,
    parsed: Option<Parsed>,
    parse_error: Option<String>,
    reparsed: bool,
    selected: usize,
    filter: String,
    status: Option<(String, bool)>,
    report: Option<Report>,
    issue_filter: IssueFilter,
    vanilla: Option<Arc<Vanilla>>,
    vanilla_task: Task<Vanilla>,
    vanilla_error: Option<String>,
    compare: String,
    act: Option<Act>,
    act_name: String,
    act_vanilla: String,
    confirm_delete: bool,
}

impl State {
    pub fn open(&mut self, path: &Path) {
        match fs::read(path) {
            Ok(bytes) => {
                self.text = String::from_utf8_lossy(&bytes).into_owned();
                self.saved = self.text.clone();
                self.path = Some(path.to_path_buf());
                self.undo.clear();
                self.selected = 0;
                self.act = None;
                self.status = None;
                self.reparse();
                if let Some(e) = &self.parse_error {
                    self.status = Some((format!("{} is not valid XML: {e}", path.display()), true));
                }
            }
            Err(e) => self.status = Some((format!("{}: {e}", path.display()), true)),
        }
    }

    pub fn compare_with(&mut self, name: &str) {
        self.compare = name.to_string();
        self.start_vanilla();
    }

    pub fn busy(&self) -> bool {
        self.vanilla_task.busy()
    }

    fn new_file(&mut self) {
        self.text = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<CHandlingDataMgr>\r\n  <HandlingData>\r\n  </HandlingData>\r\n</CHandlingDataMgr>\r\n".into();
        self.saved = String::new();
        self.path = None;
        self.undo.clear();
        self.selected = 0;
        self.status = Some(("A new, empty handling.meta. Add a vanilla vehicle's handling to start from, then Save as.".into(), false));
        self.reparse();
    }

    fn reparse(&mut self) {
        self.reparsed = true;
        match handling::parse(&self.text) {
            Ok(p) => {
                self.selected = self.selected.min(p.items.len().saturating_sub(1));
                self.parsed = Some(p);
                self.parse_error = None;
                self.report = (self.text.len() < 600_000).then(|| {
                    let label = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "handling.meta".into());
                    handling::check_sources(&[Source { label, text: self.text.clone() }])
                });
            }
            Err(e) => {
                self.parsed = None;
                self.parse_error = Some(e);
                self.report = None;
            }
        }
    }

    fn snapshot(&mut self) {
        self.undo.push(self.text.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }

    fn select_named(&mut self, name: &str) {
        if let Some(i) = self.parsed.as_ref().and_then(|p| p.items.iter().position(|h| h.name.eq_ignore_ascii_case(name))) {
            self.selected = i;
        }
    }

    fn save(&mut self, to: &Path) {
        // the first time a file is saved over, keep the one that was there
        if self.path.as_deref() == Some(to) && to.is_file() {
            let bak = to.with_extension("meta.bak");
            if !bak.exists() {
                let _ = fs::copy(to, &bak);
            }
        }
        match fs::write(to, &self.text) {
            Ok(()) => {
                self.saved = self.text.clone();
                self.path = Some(to.to_path_buf());
                self.status = Some((format!("Saved {}", to.display()), false));
            }
            Err(e) => self.status = Some((format!("Could not save {}: {e}", to.display()), true)),
        }
    }

    fn apply_text(&mut self, r: anyhow::Result<String>, note: Option<String>) {
        match r {
            Ok(t) => {
                self.snapshot();
                self.text = t;
                self.reparse();
                self.status = note.map(|n| (n, false));
            }
            Err(e) => self.status = Some((format!("{e:#}"), true)),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, game: Option<&Game>) {
        if let Some(r) = self.vanilla_task.poll(ctx) {
            match r {
                Ok(v) => self.vanilla = Some(Arc::new(v)),
                Err(e) => self.vanilla_error = Some(e),
            }
        }
        let mut stop = false;
        card(ui, "1. Your handling.meta", |ui| {
            self.toolbar(ui, game.is_some());
            if let Some((msg, err)) = &self.status {
                ui.colored_label(if *err { crate::theme::err() } else { crate::theme::ok() }, msg);
            }
            if self.text.is_empty() && self.parse_error.is_none() {
                ui.add_space(6.0);
                drop_zone(ui, "Open the handling.meta of your add-on, or start a new one");
                ui.weak("Values are changed in the file's text, so comments, spacing and number formats stay as you wrote them. The first save over a file keeps a .bak next to it.");
                stop = true;
            } else if let Some(e) = &self.parse_error {
                ui.colored_label(crate::theme::err(), format!("This is not valid XML, so it cannot be edited: {e}"));
                stop = true;
            }
        });
        if stop {
            return;
        }
        let parsed = self.parsed.take();
        let Some(p) = parsed.as_ref() else { return };
        self.reparsed = false;
        let mut pending: Option<(FieldRef, Value)> = None;
        ui.add_space(10.0);
        card(ui, "2. Vehicles", |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(250.0);
                    self.entry_list(ui, p, game.is_some());
                });
                ui.separator();
                ui.vertical(|ui| {
                    pending = self.entry_editor(ui, p);
                });
            });
        });
        if let Some((r, v)) = pending {
            match handling_edit::set_value(&self.text, r, &v) {
                Ok(t) => {
                    // one undo step per gesture (a drag is many changes): the text as it was when the gesture began
                    if let Some(before) = self.gesture_text.take() {
                        self.undo.push(before);
                        if self.undo.len() > 200 {
                            self.undo.remove(0);
                        }
                    }
                    self.text = t;
                    self.reparse();
                    self.status = None;
                }
                Err(e) => self.status = Some((format!("{e:#}"), true)),
            }
        }
        if !self.reparsed {
            self.parsed = parsed;
        }
        if let Some(r) = &self.report {
            ui.add_space(10.0);
            card(ui, "3. Check of this file", |ui| {
                ui.weak("The same checks as `crck-explorer-cli handling check`: required fields, bad numbers, duplicate names, values outside what vanilla vehicles use.");
                issues_view(ui, r, &mut self.issue_filter);
            });
        } else if self.text.len() >= 600_000 {
            ui.weak("(This file is big: it is not re-checked after every edit.)");
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, have_game: bool) {
        ui.horizontal(|ui| {
            if ui.button("Open...").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("handling.meta", &["meta", "xml"]).pick_file() {
                    self.open(&p);
                }
            }
            if ui.button("New").on_hover_text("An empty handling.meta to fill with vanilla entries").clicked() {
                self.new_file();
            }
            let has = !self.text.is_empty() && self.parse_error.is_none();
            let dirty = self.text != self.saved;
            if ui.add_enabled(has && self.path.is_some() && dirty, egui::Button::new("Save")).clicked() {
                if let Some(p) = self.path.clone() {
                    self.save(&p);
                }
            }
            if ui.add_enabled(has, egui::Button::new("Save as...")).clicked() {
                let mut d = rfd::FileDialog::new().add_filter("handling.meta", &["meta"]).set_file_name("handling.meta");
                if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
                    d = d.set_directory(dir);
                }
                if let Some(p) = d.save_file() {
                    self.save(&p);
                }
            }
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Undo the last change").clicked() {
                if let Some(t) = self.undo.pop() {
                    self.text = t;
                    self.reparse();
                    self.status = None;
                }
            }
            if ui.add_enabled(self.path.is_some() && dirty, egui::Button::new("Revert")).on_hover_text("Go back to the file as it is on disk").clicked() {
                if let Some(p) = self.path.clone() {
                    self.open(&p);
                }
            }
            if let Some(p) = &self.path {
                ui.label(egui::RichText::new(p.display().to_string()).small().color(crate::theme::dim()));
            }
            if dirty {
                ui.colored_label(crate::theme::warn(), "unsaved changes");
            }
            let _ = have_game;
        });
    }

    fn entry_list(&mut self, ui: &mut egui::Ui, p: &Parsed, have_game: bool) {
        ui.strong(format!("Vehicles ({})", p.items.len()));
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter").desired_width(f32::INFINITY));
        let f = self.filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt("handling_entries").max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, h) in p.items.iter().enumerate().filter(|(_, h)| f.is_empty() || h.name.to_lowercase().contains(&f)) {
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.selected == i, &h.name).clicked() {
                        self.selected = i;
                        self.act = None;
                        self.confirm_delete = false;
                    }
                    ui.label(egui::RichText::new(h.kind().name()).small().color(crate::theme::dim()));
                });
            }
        });
        ui.add_space(6.0);
        let name = p.items.get(self.selected).map(|h| h.name.clone());
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(name.is_some(), egui::Button::new("Copy...")).on_hover_text("Add a copy of this entry under a new name").clicked() {
                self.act = Some(Act::Copy);
                self.act_name = format!("{}_COPY", name.clone().unwrap_or_default());
            }
            if ui.add_enabled(name.is_some(), egui::Button::new("Rename...")).clicked() {
                self.act = Some(Act::Rename);
                self.act_name = name.clone().unwrap_or_default();
            }
            let label = if self.confirm_delete { "Really delete?" } else { "Delete" };
            if ui.add_enabled(name.is_some(), egui::Button::new(label)).clicked() {
                if self.confirm_delete {
                    if let Some(n) = &name {
                        let r = handling_edit::remove_entry(&self.text, n);
                        self.apply_text(r, Some(format!("Deleted {n}")));
                    }
                    self.confirm_delete = false;
                } else {
                    self.confirm_delete = true;
                }
            }
        });
        if ui.add_enabled(have_game, egui::Button::new("Add a vanilla vehicle...")).on_hover_text("Copy the handling of a vehicle from your game into this file").clicked() {
            self.act = Some(Act::AddVanilla);
            self.act_vanilla.clear();
            self.act_name.clear();
            self.start_vanilla();
        }
        if !have_game {
            ui.weak("(needs GTA V found on this PC)");
        }
        if let Some(act) = self.act {
            ui.add_space(6.0);
            ui.group(|ui| self.action_form(ui, act, name.as_deref()));
        }
    }

    fn start_vanilla(&mut self) {
        if self.vanilla.is_none() && !self.vanilla_task.busy() {
            self.vanilla_error = None;
            self.vanilla_task.run(load_vanilla);
        }
    }

    fn action_form(&mut self, ui: &mut egui::Ui, act: Act, current: Option<&str>) {
        match act {
            Act::Copy => ui.strong(format!("Copy {} as", current.unwrap_or(""))),
            Act::Rename => ui.strong("New name"),
            Act::AddVanilla => ui.strong("Add a vanilla vehicle"),
        };
        if act == Act::AddVanilla {
            self.vanilla_picker(ui, "act_vanilla");
            if self.act_name.is_empty() {
                self.act_name = self.act_vanilla.to_uppercase();
            }
            ui.label("Name in your file");
        }
        ui.add(egui::TextEdit::singleline(&mut self.act_name).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            let ok_enabled = !self.act_name.trim().is_empty() && (act != Act::AddVanilla || self.vanilla.as_ref().is_some_and(|v| v.index.contains_key(&self.act_vanilla.to_lowercase())));
            if ui.add_enabled(ok_enabled, egui::Button::new("OK")).clicked() {
                let new_name = self.act_name.trim().to_string();
                let r = match act {
                    Act::Copy => handling_edit::duplicate_entry(&self.text, current.unwrap_or(""), &new_name),
                    Act::Rename => handling_edit::rename_entry(&self.text, current.unwrap_or(""), &new_name),
                    Act::AddVanilla => match self.vanilla.as_ref().and_then(|v| v.index.get(&self.act_vanilla.to_lowercase()).map(|(si, h)| (v.clone(), *si, h.name.clone()))) {
                        Some((v, si, vname)) => handling_edit::entry_text(&v.sources[si].text, &vname).ok_or_else(|| anyhow::anyhow!("could not find {vname} in the game file")).and_then(|t| handling_edit::add_entry(&self.text, &t, &new_name)),
                        None => Err(anyhow::anyhow!("pick a vehicle from the game")),
                    },
                };
                let ok = r.is_ok();
                self.apply_text(r, Some(format!("{} {new_name}", if act == Act::Rename { "Renamed to" } else { "Added" })));
                if ok {
                    self.select_named(&new_name);
                    self.act = None;
                }
            }
            if ui.button("Cancel").clicked() {
                self.act = None;
            }
        });
    }

    /// A text field for a vanilla vehicle's name with suggestions from the game's files.
    fn vanilla_picker(&mut self, ui: &mut egui::Ui, id: &str) {
        if let Some(e) = &self.vanilla_error {
            ui.colored_label(crate::theme::err(), e);
        }
        if self.vanilla_task.busy() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Reading the game's handling files...");
            });
        }
        let target: &mut String = if id == "act_vanilla" { &mut self.act_vanilla } else { &mut self.compare };
        ui.add(egui::TextEdit::singleline(target).hint_text("vanilla vehicle, e.g. adder").desired_width(220.0));
        if let Some(v) = &self.vanilla {
            let q = target.to_lowercase();
            if !q.is_empty() && !v.index.contains_key(&q) {
                let mut names: Vec<&String> = v.index.keys().filter(|k| k.contains(&q)).collect();
                names.sort();
                ui.horizontal_wrapped(|ui| {
                    for n in names.into_iter().take(14) {
                        if ui.small_button(n).clicked() {
                            *target = n.clone();
                        }
                    }
                });
            } else if v.index.contains_key(&q) {
                ui.colored_label(crate::theme::ok(), "found in the game");
            }
        }
    }

    fn entry_editor(&mut self, ui: &mut egui::Ui, p: &Parsed) -> Option<(FieldRef, Value)> {
        let Some(item) = p.items.get(self.selected) else {
            ui.weak("No vehicles in this file yet: add a vanilla vehicle, or copy one.");
            return None;
        };
        let kind: Kind = item.kind();
        ui.horizontal(|ui| {
            ui.heading(&item.name);
            ui.label(egui::RichText::new(format!("{}  ·  line {}", kind.name(), item.line)).small().color(crate::theme::dim()));
        });
        // the vanilla vehicle shown beside every value
        ui.horizontal_wrapped(|ui| {
            ui.label("Compare with");
            if self.vanilla.is_none() {
                if ui.add_enabled(!self.vanilla_task.busy(), egui::Button::new("a vanilla vehicle...")).on_hover_text("Reads the handling files of your game (a second or two)").clicked() {
                    self.start_vanilla();
                }
                if self.vanilla_task.busy() {
                    ui.spinner();
                }
                if let Some(e) = &self.vanilla_error {
                    ui.colored_label(crate::theme::err(), e);
                }
            } else {
                self.vanilla_picker(ui, "compare");
            }
        });
        let vanilla_entry: Option<Handling> = self.vanilla.as_ref().and_then(|v| v.index.get(&self.compare.to_lowercase()).map(|(_, h)| h.clone()));
        ui.add_space(4.0);

        // every field with its place, grouped into sections
        struct Row<'a> {
            r: FieldRef,
            f: &'a handling::Field,
        }
        let mut sections: Vec<(String, Vec<Row>)> = SECTIONS.iter().map(|s| (s.to_string(), vec![])).collect();
        let mut subs: Vec<(String, Vec<Row>)> = vec![];
        for (fi, f) in item.fields.iter().enumerate() {
            let s = section_of(&f.name);
            if let Some(sec) = sections.iter_mut().find(|x| x.0 == s) {
                sec.1.push(Row { r: FieldRef { item: self.selected, sub: None, field: fi }, f });
            }
        }
        for (si, s) in item.sub.iter().enumerate() {
            if s.fields.is_empty() {
                continue;
            }
            let rows = s.fields.iter().enumerate().map(|(fi, f)| Row { r: FieldRef { item: self.selected, sub: Some(si), field: fi }, f }).collect();
            subs.push((format!("{} (specific to this kind of vehicle)", s.kind), rows));
        }
        let mut out = None;
        for (title, rows) in sections.into_iter().chain(subs) {
            if rows.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(egui::RichText::new(&title).strong()).id_salt(("hsec", &title)).default_open(true).show(ui, |ui| {
                egui::Grid::new(("hgrid", &title)).num_columns(4).spacing([14.0, 5.0]).striped(true).show(ui, |ui| {
                    for row in &rows {
                        let f = row.f;
                        let band = handling::band(&f.name, kind);
                        let outside = matches!((&f.value, band), (Value::Num(v), Some((lo, hi))) if *v < lo || *v > hi);
                        ui.label(egui::RichText::new(&f.name).monospace().color(if outside { crate::theme::warn() } else { crate::theme::text() }));
                        let (new, begin) = edit_value(ui, &f.name, &f.value);
                        if begin {
                            self.gesture_text = Some(self.text.clone());
                        }
                        if let Some(v) = new {
                            out = Some((row.r, v));
                        }
                        match band {
                            Some((lo, hi)) => {
                                ui.label(egui::RichText::new(format!("{} {}", kind.name(), range_text(lo, hi))).small().color(if outside { crate::theme::warn() } else { crate::theme::dim() })).on_hover_text("The range vanilla vehicles of this kind use (the warning band of the check)");
                            }
                            None => {
                                ui.label("");
                            }
                        }
                        // vanilla value and a button to take it over
                        match vanilla_entry.as_ref().and_then(|h| h.field(&f.name)) {
                            Some(vf) => {
                                ui.horizontal(|ui| {
                                    let differs = vf.value != f.value;
                                    ui.label(egui::RichText::new(fmt_value(&vf.value)).small().monospace().color(if differs { crate::theme::info() } else { crate::theme::dim() }));
                                    if differs && matches!(vf.value, Value::Num(_) | Value::Vec3(_) | Value::Text(_)) && !matches!(f.value, Value::Raw(_)) && ui.small_button("<-").on_hover_text("Use the vanilla value").clicked() {
                                        self.gesture_text = Some(self.text.clone());
                                        out = Some((row.r, vf.value.clone()));
                                    }
                                });
                            }
                            None => {
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/handling/good/handling.meta")
    }

    #[test]
    fn open_edit_undo_save_keeps_a_backup_and_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_hed_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("handling.meta");
        fs::copy(fixture(), &file).unwrap();
        let original = fs::read_to_string(&file).unwrap();

        let mut s = State::default();
        s.open(&file);
        assert!(s.parse_error.is_none() && s.parsed.as_ref().unwrap().items.len() == 4);
        assert!(s.report.is_some(), "the file is checked when it is opened");

        // what the UI does for a drag: remember the text, change one value, read it back
        let (item, field) = (0, s.parsed.as_ref().unwrap().items[0].fields.iter().position(|f| f.name == "fMass").unwrap());
        s.gesture_text = Some(s.text.clone());
        let new = handling_edit::set_value(&s.text, FieldRef { item, sub: None, field }, &Value::Num(1700.0)).unwrap();
        s.undo.push(s.gesture_text.take().unwrap());
        s.text = new;
        s.reparse();
        assert_eq!(s.parsed.as_ref().unwrap().items[0].num("fMass"), Some(1700.0));
        assert!(s.text != s.saved);

        s.save(&file);
        assert_eq!(fs::read_to_string(dir.join("handling.meta.bak")).unwrap(), original, "the first save keeps the old file");
        let saved = fs::read_to_string(&file).unwrap();
        let changed: Vec<_> = original.lines().zip(saved.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed.len(), 1, "only the mass line changed: {changed:?}");
        assert_eq!(original.lines().count(), saved.lines().count());
        assert!(changed[0].1.contains("1700"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copy_rename_and_delete_go_through_the_same_text_edits() {
        let mut s = State::default();
        s.open(&fixture());
        let r = handling_edit::duplicate_entry(&s.text, "FIXCAR", "MYCAR");
        s.apply_text(r, None);
        assert_eq!(s.parsed.as_ref().unwrap().items.len(), 5);
        s.select_named("mycar");
        assert_eq!(s.parsed.as_ref().unwrap().items[s.selected].name, "MYCAR");
        let r = handling_edit::remove_entry(&s.text, "MYCAR");
        s.apply_text(r, None);
        assert_eq!(s.parsed.as_ref().unwrap().items.len(), 4);
        assert_eq!(s.undo.len(), 2, "each change can be undone");
        // a bad edit is reported and changes nothing
        let before = s.text.clone();
        let r = handling_edit::duplicate_entry(&s.text, "FIXCAR", "fixbike");
        s.apply_text(r, None);
        assert!(s.status.as_ref().is_some_and(|(_, err)| *err) && s.text == before);
    }

    /// With GTA V installed: the vanilla data loads, and a vanilla vehicle can be added to a file and edited.
    #[test]
    fn a_vanilla_vehicle_can_be_added_and_changed() {
        if rage_toolkit_core::game::Game::open(None).is_err() {
            eprintln!("no GTA V install found: skipped");
            return;
        }
        let v = load_vanilla().unwrap();
        let (si, adder) = v.index.get("adder").expect("adder in the game's handling files");
        assert!(adder.num("fMass").is_some() && v.index.len() > 500);
        let mut s = State::default();
        s.new_file();
        assert_eq!(s.parsed.as_ref().unwrap().items.len(), 0);
        let text = handling_edit::entry_text(&v.sources[*si].text, &adder.name).unwrap();
        let r = handling_edit::add_entry(&s.text, &text, "MYADDER");
        s.apply_text(r, None);
        s.select_named("myadder");
        let p = s.parsed.as_ref().unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].num("fMass"), adder.num("fMass"));
        let field = p.items[0].fields.iter().position(|f| f.name == "fMass").unwrap();
        s.text = handling_edit::set_value(&s.text, FieldRef { item: 0, sub: None, field }, &Value::Num(1234.0)).unwrap();
        s.reparse();
        assert_eq!(s.parsed.as_ref().unwrap().items[0].num("fMass"), Some(1234.0));
        assert!(s.report.as_ref().unwrap().errors == 0, "a copy of a vanilla entry passes the check");
    }

    #[test]
    fn sections_cover_the_known_fields() {
        assert_eq!(section_of("fMass"), SECTIONS[0]);
        assert_eq!(section_of("fInitialDriveForce"), SECTIONS[1]);
        assert_eq!(section_of("fHandBrakeForce"), SECTIONS[2]);
        assert_eq!(section_of("fTractionCurveMax"), SECTIONS[4]);
        assert_eq!(section_of("fAntiRollBarForce"), SECTIONS[5]);
        assert_eq!(section_of("strModelFlags"), SECTIONS[7]);
        assert!(is_int("nInitialDriveGears") && !is_int("fMass"));
    }
}
