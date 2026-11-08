//! Shrink model textures: the textures embedded in every `.ydd` / `.ydr` of a folder (clothing packs carry their normal
//! maps, often ATI2, inside the models), picked by format and name, shrunk to a size cap and written to another folder
//! with the same layout. Scan first to see what would change. The same `yddtools` functions as `ydd optimize`.
use super::*;
use rage_toolkit_core::{
    texture::EncodeFormat,
    pipeline::Pow2,
    yddtools::{self, Scan, YddOptimizeOptions},
    ytdtools::OptimizeOptions,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// The formats that can be picked: label, encoder format, ticked by default.
const FORMATS: [(&str, EncodeFormat, bool); 5] = [
    ("ATI2 (BC5, normal maps)", EncodeFormat::Bc5, true),
    ("DXT5 (BC3)", EncodeFormat::Bc3, false),
    ("DXT1 (BC1)", EncodeFormat::Bc1, false),
    ("BC7", EncodeFormat::Bc7, false),
    ("ATI1 (BC4)", EncodeFormat::Bc4, false),
];
const SIZES: [u32; 6] = [2048, 1024, 512, 256, 128, 64];

/// How far a background run is: files done, files in all, the file being worked on.
#[derive(Clone, Default)]
struct Progress {
    done: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    current: Arc<Mutex<String>>,
}

impl Progress {
    fn reset(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        *self.current.lock().unwrap() = String::new();
    }
    fn callback(&self) -> impl Fn(usize, usize, &Path) {
        let p = self.clone();
        move |i, n, f| {
            p.done.store(i, Ordering::Relaxed);
            p.total.store(n, Ordering::Relaxed);
            *p.current.lock().unwrap() = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
    }
}

pub struct State {
    input: String,
    /// "" = `<input>_optimized` next to it
    out_dir: String,
    formats: [bool; 5],
    names: String,
    max_size: u32,
    target_mb: String,
    write_unchanged: bool,
    open_after: bool,
    scan_task: Task<Scan>,
    scan: Option<Result<Scan, String>>,
    run_task: Task<String>,
    output: Option<Result<String, String>>,
    progress: Progress,
}

impl Default for State {
    fn default() -> Self {
        Self {
            input: String::new(),
            out_dir: String::new(),
            formats: FORMATS.map(|f| f.2),
            names: String::new(),
            max_size: 512,
            target_mb: String::new(),
            write_unchanged: false,
            open_after: true,
            scan_task: Task::default(),
            scan: None,
            run_task: Task::default(),
            output: None,
            progress: Progress::default(),
        }
    }
}

impl State {
    /// A folder (or a model, for its folder) dropped on the window while this page is open.
    pub fn take_drop(&mut self, paths: &[PathBuf]) -> bool {
        let Some(p) = paths.first() else { return false };
        let dir = if p.is_dir() { p.clone() } else if yddtools::is_model(p) { p.parent().map(Path::to_path_buf).unwrap_or_default() } else { return false };
        self.input = dir.display().to_string();
        self.scan = None;
        self.output = None;
        true
    }

    pub fn busy(&self) -> bool {
        self.scan_task.busy() || self.run_task.busy()
    }

    fn out_path(&self) -> PathBuf {
        let o = self.out_dir.trim();
        if !o.is_empty() {
            return PathBuf::from(o);
        }
        let i = Path::new(self.input.trim());
        let name = i.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "models".into());
        i.with_file_name(format!("{name}_optimized"))
    }

    fn options(&self) -> anyhow::Result<YddOptimizeOptions> {
        let only_formats: Vec<EncodeFormat> = FORMATS.iter().zip(self.formats).filter(|(_, on)| *on).map(|(f, _)| f.1).collect();
        if only_formats.is_empty() {
            anyhow::bail!("tick at least one format");
        }
        let target = self.target_mb.trim();
        let target_bytes = if target.is_empty() { None } else { Some((target.parse::<f64>().map_err(|_| anyhow::anyhow!("the size budget '{target}' is not a number"))? * 1048576.0) as u64) };
        Ok(YddOptimizeOptions {
            base: OptimizeOptions { max_size: Some(self.max_size), pow2: Pow2::Round, drop_mips_below: None, format_override: None, target_bytes },
            only_formats,
            name_contains: self.names.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            exact_names: vec![],
            write_unchanged: self.write_unchanged,
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.scan_task.poll(ctx) {
            self.scan = Some(r);
        }
        if let Some(r) = self.run_task.poll(ctx) {
            self.output = Some(r);
        }
        ui.columns(2, |cols| {
            card(&mut cols[0], "1. Your models", |ui| {
                ui.label("A folder of .ydd / .ydr models (a stream folder, a clothing pack). Every model under it is read, sub-folders included.");
                path_field(ui, &mut self.input, Pick::Folder);
                ui.weak("or drop the folder on the window");
                ui.add_space(6.0);
                let ready = !self.input.trim().is_empty() && Path::new(self.input.trim()).is_dir();
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready && !self.busy(), egui::Button::new("Scan")).on_hover_text("Count what would be shrunk with the options on the right; nothing is written").clicked() {
                        self.start_scan();
                    }
                    if self.scan_task.busy() {
                        ui.spinner();
                        self.progress_label(ui);
                    }
                });
                self.scan_view(ui);
            });
            card(&mut cols[1], "2. What to shrink", |ui| {
                form(ui, "mt_opts", |ui| {
                    row(ui, "Formats", |ui| {
                        ui.vertical(|ui| {
                            for (k, f) in FORMATS.iter().enumerate() {
                                ui.checkbox(&mut self.formats[k], f.0);
                            }
                        });
                    });
                    row(ui, "Longest side at most", |ui| {
                        egui::ComboBox::from_id_salt("mt_size").selected_text(format!("{} px", self.max_size)).width(110.0).show_ui(ui, |ui| {
                            for s in SIZES {
                                ui.selectable_value(&mut self.max_size, s, format!("{s} px"));
                            }
                        });
                        ui.weak("bigger textures are halved until they fit");
                    });
                    row(ui, "Name contains", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.names).desired_width(160.0).hint_text("any (e.g. _normal, comma for several)"));
                    });
                    row(ui, "Budget per model", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.target_mb).desired_width(60.0).hint_text("none"));
                        ui.weak("MB of embedded textures; the biggest picked ones are halved further until it fits");
                    });
                    row(ui, "Save to", |ui| {
                        path_field(ui, &mut self.out_dir, Pick::Folder);
                    });
                });
                ui.weak(format!("empty: {}", self.out_path().display()));
                ui.checkbox(&mut self.write_unchanged, "Also copy the models that do not change (a complete copy of the folder)");
                ui.checkbox(&mut self.open_after, "Open the folder when done");
                ui.weak("The format and a full mip chain are kept. Your models are never overwritten; each written model is read back and checked first.");
                ui.add_space(8.0);
                let ready = !self.input.trim().is_empty() && Path::new(self.input.trim()).is_dir();
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready && !self.busy(), big_button("Shrink")).on_disabled_hover_text("Choose a folder of models").clicked() {
                        self.start_run();
                    }
                    if self.run_task.busy() {
                        ui.spinner();
                        self.progress_label(ui);
                    }
                });
            });
        });
        if self.output.is_some() {
            ui.add_space(10.0);
            card(ui, "Result", |ui| output_view(ui, &self.output));
        }
    }

    fn progress_label(&self, ui: &mut egui::Ui) {
        let (d, n) = (self.progress.done.load(Ordering::Relaxed), self.progress.total.load(Ordering::Relaxed));
        if n > 0 {
            ui.weak(format!("{d} / {n}  {}", self.progress.current.lock().unwrap()));
        }
    }

    fn scan_view(&self, ui: &mut egui::Ui) {
        match &self.scan {
            None => {}
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(s)) => {
                ui.add_space(6.0);
                ui.label(format!("{} model(s), {} embedded texture(s), {}", s.files, s.textures, mb(s.embedded_bytes as u64)));
                let c = if s.to_shrink > 0 { crate::theme::ok() } else { crate::theme::dim() };
                ui.label(egui::RichText::new(format!("{} texture(s) in {} model(s) would shrink: {} -> about {}", s.to_shrink, s.per_file.len(), mb(s.to_shrink_bytes as u64), mb(s.to_shrink_after as u64))).color(c));
                if s.wrong_stride > 0 {
                    ui.weak(format!("{} texture(s) have a wrong stride in their header (read correctly here; other tools show them broken)", s.wrong_stride));
                }
                if !s.errors.is_empty() {
                    ui.colored_label(crate::theme::warn(), format!("{} model(s) could not be read", s.errors.len())).on_hover_text(s.errors.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
                }
                egui::ScrollArea::vertical().id_salt("mt_scan").max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (f, n, b) in &s.per_file {
                        let rel = f.strip_prefix(self.input.trim()).unwrap_or(f);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(rel.display().to_string()).small());
                            ui.weak(format!("{n} texture(s), {}", mb(*b as u64)));
                        });
                    }
                });
            }
        }
    }

    fn start_scan(&mut self) {
        let opts = match self.options() {
            Ok(o) => o,
            Err(e) => {
                self.scan = Some(Err(format!("{e:#}")));
                return;
            }
        };
        let input = PathBuf::from(self.input.trim());
        self.progress.reset();
        let cb = self.progress.callback();
        self.scan = None;
        self.scan_task.run(move || yddtools::scan_dir(&input, &opts, &cb));
    }

    fn start_run(&mut self) {
        let opts = match self.options() {
            Ok(o) => o,
            Err(e) => {
                self.output = Some(Err(format!("{e:#}")));
                return;
            }
        };
        let (input, out, open_after) = (PathBuf::from(self.input.trim()), self.out_path(), self.open_after);
        self.progress.reset();
        let cb = self.progress.callback();
        self.output = None;
        self.run_task.run(move || {
            let results = yddtools::optimize_dir(&input, &out, &opts, &cb)?;
            let (mut written, mut changed, mut textures, mut failed) = (0, 0, 0, vec![]);
            let (mut before, mut after) = (0u64, 0u64);
            let mut lines = String::new();
            for r in results {
                match r {
                    Ok(r) => {
                        if !r.skipped {
                            written += 1;
                        }
                        if r.changed() > 0 {
                            changed += 1;
                            textures += r.changed();
                            before += r.file_before;
                            after += r.file_after;
                            lines += &yddtools::format_optimize(&r);
                        }
                    }
                    Err(e) => failed.push(format!("{e:#}")),
                }
            }
            let mut text = format!(
                "{changed} model(s) changed ({textures} texture(s)): {} -> {}; {written} model(s) written to {}\n",
                mb(before),
                mb(after),
                out.display()
            );
            if !failed.is_empty() {
                text += &format!("{} model(s) failed and were not written:\n", failed.len());
                for f in &failed {
                    text += &format!("  {f}\n");
                }
            }
            text += "\n";
            text += &lines;
            if open_after && written > 0 {
                reveal(&out);
            }
            Ok(text)
        });
    }
}
