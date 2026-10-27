//! Check a pack folder (`crck-explorer validate`).
use super::*;
use rage_toolkit_core::validate::{self, Profile};

#[derive(Default)]
pub struct State {
    folder: String,
    fivem: bool,
    task: Task<Report>,
    report: Option<Result<Report, String>>,
    filter: IssueFilter,
}

impl State {
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.task.poll(ctx) {
            self.report = Some(r);
        }
        card(ui, "1. The pack to check", |ui| self.form_card(ui));
        ui.add_space(10.0);
        card(ui, "2. What was found", |ui| match &self.report {
            None => {
                ui.weak("Pick the folder you build from (flat: model files and metas) or the finished resource, then Check.");
            }
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(r)) => issues_view(ui, r, &mut self.filter),
        });
    }

    fn form_card(&mut self, ui: &mut egui::Ui) {
        form(ui, "validate_form", |ui| {
            row(ui, "Pack folder", |ui| {
                path_field(ui, &mut self.folder, Pick::Folder);
            });
            row(ui, "Rules", |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.fivem, false, "Metas only").on_hover_text("Names that must match between vehicles, handling, carvariations, carcols and layouts, and the model files");
                    ui.radio_value(&mut self.fivem, true, "FiveM resource").on_hover_text("Also the manifest, the stream folder, the file size limits and stray files");
                });
            });
        });
        ui.add_space(6.0);
        let dir = PathBuf::from(self.folder.trim());
        if run_button(ui, "Check", dir.is_dir(), self.task.busy()) {
            let profile = if self.fivem { Profile::Fivem } else { Profile::Core };
            self.task.run(move || Ok(validate::run(&dir, profile)));
        }
    }
}
