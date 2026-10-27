//! Open RPF archives: list, extract, build (`crck-explorer list`, `extract`, `rpf`). The game's own encrypted archives are in the viewer.
use super::*;
use rage_toolkit_core::rpf;

#[derive(Default)]
pub struct State {
    rpf: String,
    listing: Task<Vec<(String, usize)>>,
    files: Option<Result<Vec<(String, usize)>, String>>,
    filter: String,
    out_dir: String,
    extract: Task<String>,
    extracted: Option<Result<String, String>>,
    build_dir: String,
    build_out: String,
    build: Task<String>,
    built: Option<Result<String, String>>,
}

impl State {
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.listing.poll(ctx) {
            self.files = Some(r);
        }
        if let Some(r) = self.extract.poll(ctx) {
            self.extracted = Some(r);
        }
        if let Some(r) = self.build.poll(ctx) {
            self.built = Some(r);
        }

        ui.columns(2, |cols| {
            card(&mut cols[0], "Look inside an archive", |ui| self.look_card(ui));
            card(&mut cols[1], "Build an archive from a folder", |ui| self.build_card(ui));
        });
    }

    fn look_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("Open archives only (what classic modding tools writes, and what CRCK Explorer builds). The game's own encrypted archives open in the viewer's Files tab.");
        form(ui, "rpf_open", |ui| {
            row(ui, "Archive", |ui| {
                path_field(ui, &mut self.rpf, Pick::OpenFile(&["rpf"]));
            });
        });
        let rpf_path = PathBuf::from(self.rpf.trim());
        ui.horizontal(|ui| {
            if run_button(ui, "List files", rpf_path.is_file(), self.listing.busy()) {
                let p = rpf_path.clone();
                self.listing.run(move || rpf::list(&p));
            }
        });
        match &self.files {
            None => {}
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(files)) => {
                ui.horizontal(|ui| {
                    ui.label(format!("{} file(s)", files.len()));
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter by name").desired_width(220.0));
                });
                let f = self.filter.to_lowercase();
                egui::ScrollArea::vertical().id_salt("rpf_files").max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (name, size) in files.iter().filter(|(n, _)| f.is_empty() || n.to_lowercase().contains(&f)) {
                        ui.horizontal(|ui| {
                            ui.label(name);
                            ui.label(egui::RichText::new(format!("{size} B")).small().color(crate::theme::dim()));
                        });
                    }
                });
                form(ui, "rpf_extract", |ui| {
                    row(ui, "Extract to", |ui| {
                        path_field(ui, &mut self.out_dir, Pick::Folder);
                    });
                });
                if run_button(ui, "Extract all", !self.out_dir.trim().is_empty(), self.extract.busy()) {
                    let (p, out) = (rpf_path.clone(), PathBuf::from(self.out_dir.trim()));
                    self.extract.run(move || {
                        let n = rpf::extract(&p, &out)?;
                        Ok(format!("{n} file(s) extracted to {} (nested archives become folders)", out.display()))
                    });
                }
                match &self.extracted {
                    Some(Ok(t)) => {
                        ui.colored_label(crate::theme::ok(), t);
                    }
                    Some(Err(e)) => {
                        ui.colored_label(crate::theme::err(), e);
                    }
                    None => {}
                }
            }
        }
    }

    fn build_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("Subfolders called *.rpf become archives inside the archive. Output is an open (unencrypted) RPF7 that classic modding tools and the reference tool read.");
        form(ui, "rpf_build", |ui| {
            row(ui, "Folder", |ui| {
                path_field(ui, &mut self.build_dir, Pick::Folder);
            });
            row(ui, "Save as", |ui| {
                path_field(ui, &mut self.build_out, Pick::SaveFile(&["rpf"]));
            });
        });
        let dir = PathBuf::from(self.build_dir.trim());
        if run_button(ui, "Build", dir.is_dir() && !self.build_out.trim().is_empty(), self.build.busy()) {
            let out = PathBuf::from(self.build_out.trim());
            self.build.run(move || {
                let bytes = rpf::build(&dir)?;
                std::fs::write(&out, &bytes)?;
                Ok(format!("{} written ({})", out.display(), mb(bytes.len() as u64)))
            });
        }
        match &self.built {
            Some(Ok(t)) => {
                ui.colored_label(crate::theme::ok(), t);
            }
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            None => {}
        }
    }
}
