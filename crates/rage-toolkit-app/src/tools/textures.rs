//! Images to YTD, the folder-to-YTD tools way: drop image files or whole folders on the page (or add them with the buttons), choose how to
//! convert (all images into one YTD, or each image into its own), choose the format, press Convert.
//! (Editing and inspecting an existing YTD is done by double-clicking it in the Explorer.)
use super::*;
use eframe::egui::{Stroke, StrokeKind};
use rage_toolkit_core::{
    pipeline::{self, YtdMode},
    texture::EncodeFormat,
    ytdtools,
};

const FORMATS: [&str; 5] = ["bc1", "bc3", "bc5", "bc7", "rgba8"];
const POW2: [(&str, &str); 4] = [("round", "Nearest power of two"), ("down", "Round down"), ("up", "Round up"), ("keep", "Leave the size alone")];

fn format_of(s: &str) -> anyhow::Result<Option<EncodeFormat>> {
    if s.is_empty() {
        return Ok(None);
    }
    EncodeFormat::parse(s).map(Some).ok_or_else(|| anyhow::anyhow!("unknown format '{s}'"))
}

fn num<T: std::str::FromStr>(s: &str, what: &str) -> anyhow::Result<Option<T>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse().map(Some).map_err(|_| anyhow::anyhow!("{what} '{s}' is not a number"))
}

/// One thing the user dropped or added, with a line saying what is in it.
struct Item {
    path: PathBuf,
    info: String,
}

fn describe(p: &Path) -> String {
    if p.is_dir() {
        match pipeline::collect_images(&[p.to_path_buf()]).len() {
            0 => "no images".into(),
            1 => "1 image".into(),
            n => format!("{n} images"),
        }
    } else {
        std::fs::metadata(p).map(|m| mb(m.len())).unwrap_or_default()
    }
}

fn is_image(p: &Path) -> bool {
    p.extension().map(|e| pipeline::IMAGE_EXT.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)
}

pub struct State {
    items: Vec<Item>,
    /// "" = automatic per image
    format: String,
    show_summary: bool,
    show_folder: bool,
    /// "" = next to the source
    out_dir: String,
    /// the user must pick one: all images into one YTD, or each into its own
    mode: Option<YtdMode>,
    /// the name of the one YTD (empty: the folder's name, or `textures`)
    one_name: String,
    max_size: String,
    pow2: String,
    budget: f64,
    task: Task<String>,
    output: Option<Result<String, String>>,
}

impl Default for State {
    fn default() -> Self {
        Self { items: vec![], format: String::new(), show_summary: true, show_folder: true, out_dir: String::new(), mode: None, one_name: String::new(), max_size: String::new(), pow2: "round".into(), budget: 16.0, task: Task::default(), output: None }
    }
}

impl State {
    /// Files and folders dropped on the window while this page is open. Returns true when any of them was taken.
    pub fn add_dropped(&mut self, paths: &[PathBuf]) -> bool {
        let mut any = false;
        for p in paths {
            if (p.is_dir() || is_image(p)) && !self.items.iter().any(|i| i.path == *p) {
                self.items.push(Item { path: p.clone(), info: describe(p) });
                any = true;
            }
        }
        if any {
            self.output = None;
        }
        any
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.task.poll(ctx) {
            self.output = Some(r);
        }
        ui.columns(2, |cols| {
            card(&mut cols[0], "1. Your images", |ui| {
                self.drop_zone(ui, ctx);
                ui.weak("Supported formats: PNG, DDS, TGA, JPG/JPEG, BMP, WEBP");
            });
            card(&mut cols[1], "2. What to make", |ui| {
                self.mode_choice(ui);
                ui.add_space(6.0);
                self.options(ui);
            });
        });
        if self.output.is_some() {
            ui.add_space(10.0);
            card(ui, "Result", |ui| output_view(ui, &self.output));
        }
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 250.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 8.0, crate::theme::surface());
        p.rect_stroke(rect, 8.0, Stroke::new(if hovering { 2.0 } else { 1.0 }, if hovering { crate::theme::info() } else { crate::theme::surface_hi() }), StrokeKind::Inside);
        let inner = rect.shrink(12.0);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)));
        let mut remove = None;
        if self.items.is_empty() {
            ui.add_space(70.0);
            ui.label(egui::RichText::new("Drop images or folders here").size(22.0).color(crate::theme::text()));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("images (.dds, .png, .tga, ...) or folders of them").color(crate::theme::dim()));
        } else {
            egui::ScrollArea::vertical().id_salt("tex_items").max_height(inner.height() - 44.0).auto_shrink([false, false]).show(&mut ui, |ui| {
                for (i, it) in self.items.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(if it.path.is_dir() { "folder" } else { "image " }).monospace().color(if it.path.is_dir() { crate::theme::warn() } else { crate::theme::info() }));
                        ui.label(it.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| it.path.display().to_string())).on_hover_text(it.path.display().to_string());
                        ui.weak(&it.info);
                        if ui.small_button("x").on_hover_text("Take it out of the list").clicked() {
                            remove = Some(i);
                        }
                    });
                }
            });
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add folder...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.add_dropped(&[d]);
                    }
                }
                if ui.button("Add images...").clicked() {
                    if let Some(fs) = rfd::FileDialog::new().add_filter("Images", &pipeline::IMAGE_EXT).pick_files() {
                        self.add_dropped(&fs);
                    }
                }
            });
        });
        if let Some(i) = remove {
            self.items.remove(i);
        }
    }

    /// The choice that has to be made before Convert works.
    fn mode_choice(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Make:");
            ui.radio_value(&mut self.mode, Some(YtdMode::All), "all images into one YTD");
            ui.radio_value(&mut self.mode, Some(YtdMode::Each), "each image into its own YTD");
            if self.mode.is_none() {
                ui.label(egui::RichText::new("choose one").color(crate::theme::warn()));
            }
        });
        if self.mode == Some(YtdMode::All) {
            ui.horizontal(|ui| {
                ui.label("YTD name:");
                ui.add(egui::TextEdit::singleline(&mut self.one_name).desired_width(160.0).hint_text("folder name, or textures"));
                ui.weak(".ytd");
            });
        }
    }

    fn options(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Format");
            let label = if self.format.is_empty() { "Automatic".to_string() } else { self.format.to_uppercase() };
            egui::ComboBox::from_id_salt("tex_fmt").selected_text(label).width(120.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.format, String::new(), "Automatic");
                for f in FORMATS {
                    ui.selectable_value(&mut self.format, f.to_string(), f.to_uppercase());
                }
            });
            ui.checkbox(&mut self.show_summary, "Show what was made");
            ui.checkbox(&mut self.show_folder, "Show output folder");
            let busy = self.task.busy();
            if ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                self.items.clear();
                self.output = None;
            }
        });
        egui::CollapsingHeader::new("More options").show(ui, |ui| {
            form(ui, "tex_more", |ui| {
                row(ui, "Save the YTDs in", |ui| {
                    path_field(ui, &mut self.out_dir, Pick::Folder);
                    ui.weak("empty: next to the images");
                });
                row(ui, "Longest side at most", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.max_size).desired_width(80.0).hint_text("any"));
                    ui.weak("pixels, e.g. 1024");
                });
                row(ui, "Sizes", |ui| {
                    let label = POW2.iter().find(|o| o.0 == self.pow2).map(|o| o.1).unwrap_or("");
                    egui::ComboBox::from_id_salt("tex_pow2").selected_text(label).width(220.0).show_ui(ui, |ui| {
                        for (v, l) in POW2 {
                            ui.selectable_value(&mut self.pow2, v.to_string(), l);
                        }
                    });
                });
                row(ui, "File size budget", |ui| {
                    ui.add(egui::DragValue::new(&mut self.budget).range(1.0..=64.0).suffix(" MB"));
                    ui.weak("FiveM streams about 16 MB per file");
                });
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.task.busy();
            if ui.add_enabled(!self.items.is_empty() && self.mode.is_some() && !busy, big_button("Convert")).on_disabled_hover_text("Add images, and choose one YTD or one each").clicked() {
                self.convert();
            }
            if busy {
                ui.spinner();
            }
        });
    }

    fn convert(&mut self) {
        let sources: Vec<PathBuf> = self.items.iter().map(|i| i.path.clone()).collect();
        let out_dir = (!self.out_dir.trim().is_empty()).then(|| PathBuf::from(self.out_dir.trim()));
        let Some(mode) = self.mode else { return };
        let one_name = self.one_name.clone();
        let (max, pow2, fmt, budget, summary, show_folder) = (self.max_size.clone(), self.pow2.clone(), self.format.clone(), self.budget, self.show_summary, self.show_folder);
        self.output = None;
        self.task.run(move || {
            let opts = pipeline::Options { max_size: num(&max, "the size limit")?, pow2: ytdtools::parse_pow2(&pow2).map_err(|e| anyhow::anyhow!("{e}"))?, format: format_of(&fmt)? };
            let jobs = pipeline::plan_ytds(&sources, out_dir.as_deref(), mode, Some(&one_name));
            if jobs.is_empty() {
                anyhow::bail!("there are no images in what you dropped ({})", pipeline::IMAGE_EXT.join(", "));
            }
            let mut text = String::new();
            let mut made: Vec<PathBuf> = vec![];
            for job in &jobs {
                if let Some(dir) = job.out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let rep = pipeline::files_to_ytd(&job.files, &job.out, &opts).map_err(|e| anyhow::anyhow!("{}: {e:#}", job.out.display()))?;
                let size = std::fs::metadata(&job.out)?.len();
                text += &format!("{}  <-  {} image(s), {}\n", job.out.display(), rep.len(), mb(size));
                if summary {
                    text += &fmt_reports(&rep);
                }
                if size as f64 > budget * 1048576.0 {
                    text += &format!("WARNING: over the {budget} MB budget. Try a smaller size limit, or split it with `crck-explorer ytd split`.\n");
                }
                text += "\n";
                made.push(job.out.clone());
            }
            text += &format!("{} YTD(s) made.\n", made.len());
            if show_folder {
                if let Some(first) = made.first() {
                    reveal(first);
                }
            }
            Ok(text)
        });
    }
}
