//! CRCK Livery: a livery `.dds` (or any picture) made into a livery `.yft`. One window, one card: the Liveries page's
//! "Livery .dds to .yft" card of CRCK Explorer as a program of its own. It calls the same function
//! (`rage_toolkit_core::livery_model::livery_yft`), and looks the same: the theme and the settings file are CRCK Explorer's own
//! source files, so a theme chosen there is the theme here.
//!
//! `CRCK-Livery.exe <template.yft> <picture> [out.yft]` does the same without a window (for scripts).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use rage_toolkit_core::livery_model;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "../../rage-toolkit-app/src/settings.rs"]
mod settings;
#[allow(dead_code)]
#[path = "../../rage-toolkit-app/src/theme.rs"]
mod theme;

const PICTURES: [&str; 6] = ["dds", "png", "jpg", "jpeg", "tga", "bmp"];

#[derive(Default)]
struct App {
    /// the vehicle's own livery file: it gives the shape (the quad, the shader, the texture's name and size)
    tpl: String,
    pic: String,
    save_as: String,
    made: Option<Result<String, String>>,
}

impl App {
    fn set_template(&mut self, p: &Path) {
        self.tpl = p.display().to_string();
        if let Some(n) = livery_model::next_name(p) {
            self.save_as = n.display().to_string();
        }
    }

    /// Files dropped on the window: a `.yft` is the template, a picture is the livery.
    fn take_drop(&mut self, paths: &[PathBuf]) {
        for p in paths {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext == "yft" {
                self.set_template(p);
            } else if PICTURES.contains(&ext.as_str()) {
                self.pic = p.display().to_string();
            }
        }
    }

    fn make(&mut self) {
        let (t, p, o) = (PathBuf::from(self.tpl.trim()), PathBuf::from(self.pic.trim()), PathBuf::from(self.save_as.trim()));
        self.made = Some(livery_model::livery_yft(&t, &p, &o).map_err(|e| format!("{e:#}")));
        if self.made.as_ref().is_some_and(Result::is_ok) {
            // the next one: the same template, the next free name
            if let Some(n) = livery_model::next_name(&t) {
                self.save_as = n.display().to_string();
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.take_drop(&dropped);
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(28, 20))).show(ui, |ui| {
            ui.label(egui::RichText::new("Livery .dds to .yft").size(22.0).strong());
            ui.label(egui::RichText::new("Your picture becomes a livery file the game can load.").color(theme::dim()));
            ui.add_space(12.0);
            card(ui, "1  The vehicle's livery file", |ui| {
                ui.weak("One of the vehicle's own livery files (like polb2_livery1.yft). It gives the shape; it is not changed.");
                ui.add_space(4.0);
                if path_field(ui, &mut self.tpl, Pick::OpenFile(&["yft"])) {
                    let p = PathBuf::from(self.tpl.trim());
                    self.set_template(&p);
                }
            });
            ui.add_space(10.0);
            card(ui, "2  Your livery", |ui| {
                ui.weak("A .dds, or a .png, .jpg, .tga or .bmp. It is made the same size and format as the template's texture when it is not already.");
                ui.add_space(4.0);
                path_field(ui, &mut self.pic, Pick::OpenFile(&PICTURES));
            });
            ui.add_space(10.0);
            card(ui, "3  Save as", |ui| {
                path_field(ui, &mut self.save_as, Pick::SaveFile(&["yft"]));
                let ready = Path::new(self.tpl.trim()).is_file() && Path::new(self.pic.trim()).is_file() && !self.save_as.trim().is_empty();
                ui.add_space(8.0);
                if ui.add_enabled(ready, big_button("Make the .yft")).on_disabled_hover_text("Choose the template, the picture and where to save first").clicked() {
                    self.make();
                }
                match &self.made {
                    Some(Ok(m)) => drop(ui.colored_label(theme::ok(), m)),
                    Some(Err(e)) => drop(ui.colored_label(theme::err(), e)),
                    None => {}
                }
            });
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Or drop the .yft and the picture anywhere on this window.").color(theme::dim()));
        });
    }
}

// ---- the pieces every CRCK Explorer tool page is made of (crates/rage-toolkit-app/src/tools/mod.rs) --------------------

/// What a Browse... button asks for.
#[derive(Clone, Copy)]
enum Pick {
    OpenFile(&'static [&'static str]),
    SaveFile(&'static [&'static str]),
}

/// A text field with a Browse... button. True when the dialog chose a new path.
fn path_field(ui: &mut egui::Ui, value: &mut String, pick: Pick) -> bool {
    let mut chosen = false;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width((ui.available_width() - 92.0).max(120.0)));
        if ui.button("Browse...").clicked() {
            let mut d = rfd::FileDialog::new();
            if let Some(s) = Path::new(value.as_str()).parent().filter(|p| p.is_dir()) {
                d = d.set_directory(s);
            }
            let picked = match pick {
                Pick::OpenFile(ext) => d.add_filter("Files", ext).pick_file(),
                Pick::SaveFile(ext) => d.add_filter("Files", ext).save_file(),
            };
            if let Some(p) = picked {
                *value = p.display().to_string();
                chosen = true;
            }
        }
    });
    chosen
}

/// A rounded card with a title.
fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(theme::surface()).stroke(egui::Stroke::new(1.0, theme::border())).corner_radius(8.0).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).size(16.0).strong());
        ui.add_space(6.0);
        add(ui);
    });
}

/// The main action: big, capitals.
fn big_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label.to_uppercase()).strong().size(14.0)).min_size(egui::vec2(180.0, 32.0))
}

fn window_icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../../../assets/icon-256.png")) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            egui::IconData { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }
        }
        Err(_) => egui::IconData::default(),
    }
}

/// The theme chosen in CRCK Explorer (its settings file), else the default one.
fn chosen_theme() -> theme::Tokens {
    let wanted = settings::get(&settings::path(), "theme").unwrap_or_else(|| theme::DEFAULT.to_string());
    let mut all = theme::builtin();
    all.extend(theme::load_custom(&theme::dir()));
    all.iter().find(|t| t.name == wanted).or_else(|| all.iter().find(|t| t.name == theme::DEFAULT)).map(|t| t.tokens).unwrap_or(theme::GRAPHITE)
}

fn main() -> eframe::Result {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.len() >= 2 {
        // no window: template, picture, and where to save (else the next free livery name beside the template)
        let out = args.get(2).cloned().or_else(|| livery_model::next_name(&args[0]));
        let done = out.ok_or_else(|| "no name to save as: give it as the third argument".to_string()).and_then(|o| livery_model::livery_yft(&args[0], &args[1], &o).map_err(|e| format!("{e:#}")));
        match done {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let mut app = App::default();
    app.take_drop(&args); // a file dragged onto the .exe
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([620.0, 590.0]).with_min_inner_size([460.0, 560.0]).with_title("CRCK Livery").with_drag_and_drop(true).with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native("CRCK Livery", options, Box::new(move |cc| {
        theme::apply(&cc.egui_ctx, &chosen_theme());
        Ok(Box::new(app))
    }))
}
