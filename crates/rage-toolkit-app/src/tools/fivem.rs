//! The add-on vehicle creator (`crck-explorer pack`): model files + metas in, a FiveM resource or a single-player DLC folder out.
use super::*;
use rage_toolkit_core::{
    pack::{self, Mode},
    validate::{self, Profile},
};

/// What is in the source folder, for the checklist.
struct Analysis {
    models: Vec<(String, usize)>,
    metas: Vec<(&'static str, bool)>,
    other: usize,
}

fn analyse(src: &Path) -> Option<Analysis> {
    let files: Vec<String> = std::fs::read_dir(src).ok()?.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_lowercase()).collect();
    let models: Vec<(String, usize)> = ["yft", "ytd", "ydd", "ydr", "ybn"].iter().map(|e| (e.to_string(), files.iter().filter(|f| f.ends_with(&format!(".{e}"))).count())).filter(|(_, n)| *n > 0).collect();
    let metas: Vec<(&'static str, bool)> = pack::METAS.iter().map(|(f, _)| (*f, files.iter().any(|x| x == f))).collect();
    let used = models.iter().map(|m| m.1).sum::<usize>() + metas.iter().filter(|m| m.1).count();
    Some(Analysis { models, metas, other: files.len().saturating_sub(used) })
}

struct Built {
    folder: PathBuf,
    mode: Mode,
    report: Option<Report>,
    missing: Option<usize>,
}

pub struct State {
    name: String,
    src: String,
    out: String,
    mode: Mode,
    analysed: (String, Option<Analysis>),
    task: Task<Built>,
    result: Option<Result<Built, String>>,
    filter: IssueFilter,
}

impl Default for State {
    fn default() -> Self {
        Self { name: String::new(), src: String::new(), out: String::new(), mode: Mode::Fivem, analysed: (String::new(), None), task: Task::default(), result: None, filter: IssueFilter::default() }
    }
}

/// What each meta is for, and whether a vehicle needs it.
fn meta_role(f: &str) -> (&'static str, bool) {
    match f {
        "vehicles.meta" => ("the vehicle: model, names, texture parents", true),
        "handling.meta" => ("how it drives", true),
        "carcols.meta" => ("colours, sirens, mod kits", false),
        "carvariations.meta" => ("colour combinations, liveries", false),
        "vehiclelayouts.meta" => ("seats and animations", false),
        _ => ("", false),
    }
}

impl State {
    /// A folder dropped on the window while this page is open: the vehicle's files.
    pub fn take_drop(&mut self, paths: &[PathBuf]) -> bool {
        let Some(d) = paths.iter().find(|p| p.is_dir()) else { return false };
        self.src = d.display().to_string();
        self.fill_defaults();
        true
    }

    /// a name and an output folder to start from, after the source folder is chosen
    fn fill_defaults(&mut self) {
        let p = PathBuf::from(&self.src);
        if self.name.is_empty() {
            self.name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
        if self.out.is_empty() {
            self.out = p.parent().map(|d| d.join("rage_toolkit_output").display().to_string()).unwrap_or_default();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.task.poll(ctx) {
            self.result = Some(r);
        }
        if self.analysed.0 != self.src {
            self.analysed = (self.src.clone(), analyse(Path::new(self.src.trim())));
        }
        ui.columns(2, |cols| {
            card(&mut cols[0], "1. Your vehicle's files", |ui| self.files_card(ui));
            card(&mut cols[1], "2. What to make", |ui| self.make_card(ui));
        });
        ui.add_space(10.0);
        self.result_card(ui);
    }

    fn files_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("One folder with the model files (.yft .ytd .ydd .ydr .ybn) and the vehicle's .meta files. Drop it on the window, or:");
        if path_field(ui, &mut self.src, Pick::Folder) {
            self.fill_defaults();
        }
        ui.add_space(8.0);
        let tick = |ui: &mut egui::Ui, ok: bool, needed: bool, text: String, note: &str| {
            ui.horizontal(|ui| {
                let (mark, colour) = if ok { ("OK", crate::theme::ok()) } else if needed { ("--", crate::theme::err()) } else { ("--", crate::theme::dim()) };
                ui.label(egui::RichText::new(mark).monospace().strong().color(colour));
                ui.label(egui::RichText::new(text).color(if ok { crate::theme::text() } else { colour }));
                ui.weak(note);
            });
        };
        match &self.analysed.1 {
            None if !self.src.trim().is_empty() => {
                ui.colored_label(crate::theme::err(), "That folder cannot be read.");
            }
            None => {
                drop_zone(ui, "Drop the vehicle's folder here");
            }
            Some(a) => {
                let n: usize = a.models.iter().map(|m| m.1).sum();
                let models = if n == 0 { "no model files".to_string() } else { format!("{n} model file(s): {}", a.models.iter().map(|(e, c)| format!("{c} .{e}")).collect::<Vec<_>>().join(", ")) };
                tick(ui, n > 0, true, models, "");
                for (f, found) in &a.metas {
                    let (role, needed) = meta_role(f);
                    tick(ui, *found, needed, f.to_string(), &format!("{role}{}", if *found || needed { "" } else { " (optional)" }));
                }
                if a.other > 0 {
                    ui.weak(format!("{} other file(s) in the folder are left out.", a.other));
                }
                if !a.metas.iter().any(|m| m.0 == "handling.meta" && m.1) {
                    ui.weak("No handling.meta? The Handling editor can copy a vanilla vehicle's.");
                }
            }
        }
    }

    fn make_card(&mut self, ui: &mut egui::Ui) {
        form(ui, "pack_out", |ui| {
            row(ui, "Name", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(220.0));
                ui.weak(format!("folder: {}", pack::sanitize(&self.name)));
            });
            row(ui, "Output folder", |ui| {
                path_field(ui, &mut self.out, Pick::Folder);
            });
        });
        ui.add_space(8.0);
        ui.label("Kind");
        ui.vertical(|ui| {
            for (mode, title, what) in [(Mode::Fivem, "FiveM resource", "fxmanifest.lua, stream/ and data/: drop it in your server's resources"), (Mode::Sp, "Single-player DLC", "dlcpacks/<name>/dlc.rpf as folders, to pack, plus the dlclist line")] {
                let on = self.mode == mode;
                let r = egui::Frame::new()
                    .fill(if on { crate::theme::accent().gamma_multiply(0.22) } else { crate::theme::surface_hi() })
                    .stroke(egui::Stroke::new(if on { 2.0 } else { 1.0 }, if on { crate::theme::accent() } else { crate::theme::border() }))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            ui.weak(what);
                        });
                    })
                    .response
                    .interact(egui::Sense::click());
                if r.clicked() {
                    self.mode = mode;
                }
            }
        });
        ui.add_space(8.0);
        let target = Path::new(self.out.trim()).join(pack::sanitize(&self.name));
        if !self.out.trim().is_empty() && !self.name.trim().is_empty() {
            ui.weak(format!("Will be written to {}", target.display()));
        }
        let (src, out) = (PathBuf::from(self.src.trim()), PathBuf::from(self.out.trim()));
        let ready = src.is_dir() && !self.out.trim().is_empty() && !self.name.trim().is_empty() && self.analysed.1.as_ref().is_some_and(|a| a.models.iter().map(|m| m.1).sum::<usize>() > 0);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let b = big_button("Create the pack");
            if ui.add_enabled(ready && !self.task.busy(), b).on_disabled_hover_text("Choose the folder with the model files, a name and an output folder").clicked() {
                let (name, mode) = (self.name.clone(), self.mode);
                self.task.run(move || {
                    let folder = pack::build(mode, &name, &src, &out)?;
                    // check what was made, the way the CLI's validate / verify would
                    let (report, missing) = match mode {
                        Mode::Fivem => (Some(validate::run(&folder, Profile::Fivem)), None),
                        Mode::Sp => (None, pack::verify(&folder).ok()),
                    };
                    Ok(Built { folder, mode, report, missing })
                });
            }
            if self.task.busy() {
                ui.spinner();
            }
        });
    }

    fn result_card(&mut self, ui: &mut egui::Ui) {
        let Some(result) = &self.result else { return };
        match result {
            Err(e) => {
                let e = e.clone();
                card(ui, "The pack was not made", |ui| {
                    ui.colored_label(crate::theme::err(), e);
                });
            }
            Ok(b) => {
                let (folder, mode, missing) = (b.folder.clone(), b.mode, b.missing);
                card(ui, "Made", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(folder.display().to_string());
                        if ui.button("Open folder").clicked() {
                            reveal(&folder);
                        }
                    });
                    match mode {
                        Mode::Fivem => {
                            ui.weak("Drop the folder into your server's resources and add `ensure <name>` to server.cfg. Checked with the FiveM rules:");
                        }
                        Mode::Sp => {
                            ui.weak("Next: pack x64/vehicles.rpf and dlc.rpf (Archives page, classic modding tools or the reference tool), put the result under mods/update/x64/dlcpacks/<name>/, and add the line in dlclist_entry.txt to dlclist.xml.");
                            match missing {
                                Some(0) => {
                                    ui.colored_label(crate::theme::ok(), "Every path in content.xml exists in the tree.");
                                }
                                Some(n) => {
                                    ui.colored_label(crate::theme::err(), format!("{n} path(s) in content.xml are missing from the tree."));
                                }
                                None => {}
                            }
                        }
                    }
                });
                if let Some(Ok(b)) = &self.result {
                    if let Some(r) = &b.report {
                        issues_view(ui, r, &mut self.filter);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checklist_counts_models_and_metas() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_packui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["car.yft", "car.ytd", "car_hi.yft", "handling.meta", "vehicles.meta", "notes.txt"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let a = analyse(&dir).unwrap();
        assert_eq!(a.models, vec![("yft".to_string(), 2), ("ytd".to_string(), 1)]);
        assert_eq!(a.metas.iter().filter(|m| m.1).map(|m| m.0).collect::<Vec<_>>(), ["vehicles.meta", "handling.meta"]);
        assert_eq!(a.other, 1);
        assert!(analyse(&dir.join("nope")).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
