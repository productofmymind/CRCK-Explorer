//! The Settings page: general switches and the theme editor. Colours are edited live (the whole application changes as you drag),
//! and only Save writes a custom theme; choosing a theme is remembered across restarts.
use crate::{
    settings,
    theme::{self, Theme, Tokens},
};
use eframe::egui::{self, RichText};

pub struct Page {
    /// built-in themes first, then the custom ones
    themes: Vec<Theme>,
    /// name of the theme the working colours started from
    pub active: String,
    working: Tokens,
    name: String,
    message: Option<Result<String, String>>,
    confirm_delete: bool,
    rename_to: String,
    /// warn when working in the game's own files instead of the "mods" folder
    pub warn_game_files: bool,
    /// the "mods" folder chosen here; None = `<game>\mods`
    custom_mods_dir: Option<std::path::PathBuf>,
}

impl Page {
    pub fn new() -> Self {
        let mut themes = theme::builtin();
        themes.extend(theme::load_custom(&theme::dir()));
        let wanted = settings::get(&settings::path(), "theme").unwrap_or_else(|| theme::DEFAULT.to_string());
        let t = themes.iter().find(|t| t.name == wanted).or_else(|| themes.iter().find(|t| t.name == theme::DEFAULT)).cloned().unwrap_or_else(|| theme::builtin().remove(1));
        let file = settings::path();
        Self {
            active: t.name.clone(),
            working: t.tokens,
            themes,
            name: String::new(),
            message: None,
            confirm_delete: false,
            rename_to: String::new(),
            warn_game_files: settings::warn_game_files(&file),
            custom_mods_dir: settings::mods_dir(&file),
        }
    }

    /// The "mods" folder in use: the one chosen here, else `<game>\mods`.
    pub fn mods_dir(&self, game_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
        self.custom_mods_dir.clone().or_else(|| game_dir.map(rage_toolkit_core::mods::default_dir))
    }

    /// Settings > Game files: the warning switch and the "mods" folder.
    fn game_files_ui(&mut self, ui: &mut egui::Ui, game_dir: Option<&std::path::Path>) {
        section(ui, "Game files and the \"mods\" folder");
        let tip = "On: a red notice shows in the Explorer while you are inside one of the game's own archives, and CRCK Explorer asks before it opens one of the game's own loose files for editing. Off: no notice and no question.";
        if ui.checkbox(&mut self.warn_game_files, "Warn me when I work in the game's own files instead of the \"mods\" folder").on_hover_text(tip).changed() {
            settings::set(&settings::path(), "warn_game_files", if self.warn_game_files { "true" } else { "false" });
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("\"mods\" folder:");
            match self.mods_dir(game_dir) {
                Some(d) => ui.label(RichText::new(d.display().to_string()).monospace()),
                None => ui.label(RichText::new("GTA V was not found: choose a folder").color(theme::dim())),
            };
            if self.custom_mods_dir.is_none() && game_dir.is_some() {
                ui.label(RichText::new("(default)").color(theme::dim()));
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Change...").on_hover_text("Choose the folder that copies of game archives go to").clicked() {
                if let Some(d) = rfd::FileDialog::new().set_title("The \"mods\" folder").pick_folder() {
                    settings::set(&settings::path(), "mods_dir", &d.display().to_string());
                    self.custom_mods_dir = Some(d);
                }
            }
            if ui.add_enabled(self.custom_mods_dir.is_some(), egui::Button::new("Use the default")).on_hover_text("<game folder>\\mods, where classic modding tools puts it").clicked() {
                settings::set(&settings::path(), "mods_dir", "");
                self.custom_mods_dir = None;
            }
        });
        ui.weak("Copy to \"mods\" folder puts a copy of the game's archive here, at the same path as in the game, and the game's own file stays untouched. The game only reads the mods folder when the mods-folder ASI plugin and an ASI loader are installed; CRCK Explorer installs neither.");
    }

    /// Use a theme without remembering it (automated screenshots).
    pub fn use_theme(&mut self, name: &str) {
        if let Some(t) = self.themes.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
            self.active = t.name.clone();
            self.working = t.tokens;
        }
    }

    pub fn tokens(&self) -> Tokens {
        self.working
    }

    fn is_builtin(&self, name: &str) -> bool {
        theme::builtin().iter().any(|b| b.name == name)
    }

    fn select(&mut self, ctx: &egui::Context, name: &str) {
        if let Some(t) = self.themes.iter().find(|t| t.name == name).cloned() {
            self.active = t.name.clone();
            self.working = t.tokens;
            self.confirm_delete = false;
            theme::apply(ctx, &self.working);
            settings::set(&settings::path(), "theme", &t.name);
        }
    }

    fn modified(&self) -> bool {
        self.themes.iter().find(|t| t.name == self.active).map(|t| t.tokens != self.working).unwrap_or(true)
    }

    /// Keep the working colours as the custom theme `name`.
    fn store(&mut self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.message = Some(Err("Give the theme a name first.".into()));
            return;
        }
        if self.is_builtin(&name) {
            self.message = Some(Err(format!("\"{name}\" is a built-in theme: choose another name.")));
            return;
        }
        let t = Theme { name: name.clone(), tokens: self.working };
        match theme::save_custom(&theme::dir(), &t) {
            Ok(()) => {
                match self.themes.iter_mut().find(|x| x.name == name) {
                    Some(x) => *x = t,
                    None => self.themes.push(t),
                }
                self.active = name.clone();
                settings::set(&settings::path(), "theme", &name);
                self.message = Some(Ok(format!("Saved the theme \"{name}\".")));
            }
            Err(e) => self.message = Some(Err(format!("Could not save: {e}"))),
        }
    }

    fn rename(&mut self) {
        let (old, new) = (self.active.clone(), self.rename_to.trim().to_string());
        if self.is_builtin(&new) || self.themes.iter().any(|t| t.name == new) {
            self.message = Some(Err(format!("A theme called \"{new}\" exists already.")));
            return;
        }
        theme::delete_custom(&theme::dir(), &old);
        if let Some(t) = self.themes.iter_mut().find(|t| t.name == old) {
            t.name = new.clone();
        }
        self.store(&new);
        self.rename_to.clear();
        self.message = Some(Ok(format!("Renamed \"{old}\" to \"{new}\".")));
    }

    fn import(&mut self, ctx: &egui::Context) {
        let Some(f) = rfd::FileDialog::new().add_filter("CRCK Explorer theme", &["theme"]).pick_file() else { return };
        match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| theme::parse(&t)) {
            Ok(mut t) => {
                // never overwrite a built-in or an existing theme silently
                let base = t.name.clone();
                let mut n = 2;
                while self.is_builtin(&t.name) || self.themes.iter().any(|x| x.name == t.name) {
                    t.name = format!("{base} ({n})");
                    n += 1;
                }
                self.working = t.tokens;
                self.store(&t.name);
                theme::apply(ctx, &self.working);
            }
            Err(e) => self.message = Some(Err(format!("{}: {e}", f.display()))),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, show_home: &mut bool, game_dir: Option<&std::path::Path>) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            ui.add_space(6.0);
            ui.label(RichText::new("Settings").size(24.0).strong());
            ui.add_space(10.0);

            section(ui, "General");
            if ui.checkbox(show_home, "Show the Home screen when CRCK Explorer starts").on_hover_text("Off: CRCK Explorer starts in the Explorer. The Home tab brings the screen back.").changed() {
                settings::save_show_home(&settings::path(), *show_home);
            }
            ui.add_space(14.0);

            self.game_files_ui(ui, game_dir);
            ui.add_space(14.0);

            section(ui, "Theme");
            ui.weak("Pick a theme, or change any colour: the whole application follows while you edit.");
            ui.add_space(4.0);
            let names: Vec<(String, bool)> = self.themes.iter().map(|t| (t.name.clone(), self.is_builtin(&t.name))).collect();
            ui.horizontal_wrapped(|ui| {
                for (n, builtin) in &names {
                    let on = *n == self.active;
                    let label = if *builtin { n.clone() } else { format!("{n}  (custom)") };
                    if ui.selectable_label(on, label).clicked() && !on {
                        self.select(&ctx, n);
                    }
                }
            });
            ui.add_space(10.0);
            ui.columns(2, |cols| {
                // the colours
                let ui = &mut cols[0];
                let mut changed = false;
                egui::Grid::new("theme_colours").num_columns(3).spacing([14.0, 8.0]).show(ui, |ui| {
                    for (_, label, col) in self.working.fields() {
                        ui.label(label);
                        changed |= egui::color_picker::color_edit_button_srgba(ui, col, egui::color_picker::Alpha::Opaque).changed();
                        ui.label(RichText::new(theme::to_hex(*col)).monospace().color(theme::dim()));
                        ui.end_row();
                    }
                });
                if changed {
                    theme::apply(&ctx, &self.working);
                }
                ui.add_space(8.0);
                if self.modified() {
                    ui.label(RichText::new("Changed: not saved yet").color(theme::warn()));
                    if ui.button("Undo my changes").clicked() {
                        let a = self.active.clone();
                        if self.themes.iter().any(|t| t.name == a) {
                            self.select(&ctx, &a);
                        }
                    }
                }
                // the preview
                preview(&mut cols[1], &self.working);
            });

            ui.add_space(14.0);
            ui.label(RichText::new("Your themes").strong());
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("name for a new theme").desired_width(200.0));
                if ui.button("Save as new theme").on_hover_text("Keeps the colours above under this name").clicked() {
                    let n = self.name.clone();
                    self.store(&n);
                    if matches!(self.message, Some(Ok(_))) {
                        self.name.clear();
                    }
                }
                let custom = !self.is_builtin(&self.active);
                if ui.add_enabled(custom && self.modified(), egui::Button::new("Save changes")).on_hover_text("Overwrites the custom theme that is selected").clicked() {
                    let n = self.active.clone();
                    self.store(&n);
                }
            });
            let custom = !self.is_builtin(&self.active);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(custom, egui::TextEdit::singleline(&mut self.rename_to).hint_text("new name").desired_width(200.0));
                if ui.add_enabled(custom && !self.rename_to.trim().is_empty(), egui::Button::new("Rename")).clicked() {
                    self.rename();
                }
                if self.confirm_delete {
                    if ui.add_enabled(custom, egui::Button::new(RichText::new("Really delete").color(theme::err()))).clicked() {
                        let old = self.active.clone();
                        theme::delete_custom(&theme::dir(), &old);
                        self.themes.retain(|t| t.name != old);
                        self.select(&ctx, theme::DEFAULT);
                        self.message = Some(Ok(format!("Deleted \"{old}\".")));
                    }
                    if ui.button("Keep it").clicked() {
                        self.confirm_delete = false;
                    }
                } else if ui.add_enabled(custom, egui::Button::new("Delete")).clicked() {
                    self.confirm_delete = true;
                }
                ui.separator();
                if ui.button("Export...").on_hover_text("Write the colours above to a .theme file to share").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("CRCK Explorer theme", &["theme"]).set_file_name(format!("{}.theme", self.active)).save_file() {
                        let t = Theme { name: self.active.clone(), tokens: self.working };
                        self.message = Some(std::fs::write(&f, theme::serialise(&t)).map(|_| format!("Exported to {}", f.display())).map_err(|e| e.to_string()));
                    }
                }
                if ui.button("Import...").on_hover_text("Add a .theme file somebody shared").clicked() {
                    self.import(&ctx);
                }
            });
            match &self.message {
                Some(Ok(t)) => ui.label(RichText::new(t).color(theme::ok())),
                Some(Err(t)) => ui.label(RichText::new(t).color(theme::err())),
                None => ui.label(""),
            };
            ui.weak(format!("Custom themes are kept in {}", theme::dir().display()));
        });
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(16.0).strong().color(theme::accent()));
    ui.add_space(4.0);
}

/// A small mock of the interface drawn with the colours being edited.
fn preview(ui: &mut egui::Ui, t: &Tokens) {
    let w = ui.available_width().min(380.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 250.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 10.0, t.bg);
    p.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    // navigation bar
    let bar = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(w - 16.0, 28.0));
    p.rect_filled(bar, 7.0, t.surface);
    for (i, n) in ["Home", "Explorer", "Viewer", "Tools", "Settings"].iter().enumerate() {
        let r = egui::Rect::from_min_size(bar.min + egui::vec2(6.0 + i as f32 * 66.0, 3.0), egui::vec2(62.0, 22.0));
        if i == 4 {
            p.rect_filled(r, 6.0, t.accent.gamma_multiply(0.3));
        }
        p.text(r.center(), egui::Align2::CENTER_CENTER, n, egui::FontId::proportional(12.5), if i == 4 { t.text } else { t.dim });
    }
    // a card
    let card = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 46.0), egui::vec2(w - 16.0, 74.0));
    p.rect_filled(card, 9.0, t.surface);
    p.rect_stroke(card, 9.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let tile = egui::Rect::from_min_size(card.min + egui::vec2(12.0, 14.0), egui::vec2(46.0, 46.0));
    p.rect_filled(tile, 10.0, t.accent.gamma_multiply(0.22));
    p.circle_stroke(tile.center(), 10.0, egui::Stroke::new(1.8, t.accent));
    p.text(card.min + egui::vec2(72.0, 24.0), egui::Align2::LEFT_CENTER, "Archetype editor", egui::FontId::proportional(15.0), t.text);
    p.text(card.min + egui::vec2(72.0, 44.0), egui::Align2::LEFT_CENTER, "Secondary text looks like this", egui::FontId::proportional(12.0), t.dim);
    // a list with a selected row and a button
    let list = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 130.0), egui::vec2(w - 16.0 - 110.0, 112.0));
    p.rect_filled(list, 8.0, t.surface);
    for (i, n) in ["BattlEye", "mods", "common.rpf", "x64a.rpf"].iter().enumerate() {
        let r = egui::Rect::from_min_size(list.min + egui::vec2(4.0, 4.0 + i as f32 * 26.0), egui::vec2(list.width() - 8.0, 24.0));
        if i == 1 {
            p.rect_filled(r, 5.0, t.accent.gamma_multiply(0.3));
        } else if i == 2 {
            p.rect_filled(r, 5.0, t.surface_hi);
        }
        p.text(r.min + egui::vec2(10.0, 12.0), egui::Align2::LEFT_CENTER, n, egui::FontId::proportional(13.0), t.text);
    }
    let b = egui::Rect::from_min_size(egui::pos2(list.right() + 10.0, list.top()), egui::vec2(92.0, 26.0));
    p.rect_filled(b, 6.0, t.surface_hi);
    p.rect_stroke(b, 6.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    p.text(b.center(), egui::Align2::CENTER_CENTER, "Button", egui::FontId::proportional(13.0), t.text);
    for (i, (txt, col)) in [("Success", t.ok), ("Warning", t.warn), ("Error", t.error), ("Info", t.info)].into_iter().enumerate() {
        p.text(egui::pos2(list.right() + 12.0, list.top() + 42.0 + i as f32 * 20.0), egui::Align2::LEFT_CENTER, txt, egui::FontId::proportional(13.0), col);
    }
}
