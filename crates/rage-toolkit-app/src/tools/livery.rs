//! Livery kits of a FiveM vehicle resource (`crck-explorer-cli livery list | add | check`), and a livery .dds made into a .yft.
use super::*;
use rage_toolkit_core::livery::{self, AddParams, KitInfo};

#[derive(Default)]
pub struct State {
    pack: String,
    kits: Option<Result<Vec<KitInfo>, String>>,
    vehicle: String,
    name: String,
    display: String,
    label: String,
    kit_id: String,
    yft: String,
    added: Option<Result<Vec<String>, String>>,
    old: String,
    new: String,
    checked: Option<Result<Vec<String>, String>>,
    /// a livery model from a picture: the template livery .yft, the picture, where to save
    tpl: String,
    pic: String,
    save_as: String,
    made: Option<Result<String, String>>,
}

fn opt(s: &str) -> Option<&str> {
    Some(s.trim()).filter(|s| !s.is_empty())
}

const PICTURES: [&str; 6] = ["dds", "png", "jpg", "jpeg", "tga", "bmp"];

impl State {
    fn reload(&mut self) {
        self.kits = Some(livery::list(Path::new(self.pack.trim())).map_err(|e| format!("{e:#}")));
    }

    fn set_template(&mut self, p: &Path) {
        self.tpl = p.display().to_string();
        if self.save_as.is_empty() {
            if let Some(n) = rage_toolkit_core::livery_model::next_name(p) {
                self.save_as = n.display().to_string();
            }
        }
    }

    /// Files dropped on the window while this page is open: a resource folder, a template .yft, a .dds picture.
    pub fn take_drop(&mut self, paths: &[PathBuf]) -> bool {
        let mut took = false;
        for p in paths {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if p.is_dir() {
                self.pack = p.display().to_string();
                self.reload();
            } else if ext == "yft" {
                self.set_template(p);
            } else if PICTURES.contains(&ext.as_str()) {
                self.pic = p.display().to_string();
            } else {
                continue;
            }
            took = true;
        }
        took
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.columns(2, |cols| {
            card(&mut cols[0], "1. Your vehicle resource", |ui| self.kits_card(ui));
            card(&mut cols[1], "2. Add a livery", |ui| self.add_card(ui));
        });
        ui.add_space(10.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], "Livery .dds to .yft", |ui| self.dds_card(ui));
            card(&mut cols[1], "Check an edit against the live server's file", |ui| self.check_card(ui));
        });
    }

    fn kits_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("The folder with fxmanifest.lua, data/ and stream/. Drop it on the window, or:");
        if path_field(ui, &mut self.pack, Pick::Folder) {
            self.reload();
            self.added = None;
        }
        ui.add_space(8.0);
        match &self.kits {
            None => drop_zone(ui, "Drop the resource folder here"),
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(kits)) if kits.is_empty() => {
                ui.colored_label(crate::theme::warn(), "No livery kits in this resource yet (the first Add livery creates one).");
            }
            Some(Ok(kits)) => {
                for k in kits {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(&k.kit);
                        ui.label(egui::RichText::new(format!("id {}  {}", k.id, k.kit_type)).small().color(crate::theme::dim()));
                        if !k.linked_to.is_empty() {
                            ui.label(egui::RichText::new(format!("used by {}", k.linked_to.join(", "))).small().color(crate::theme::info()));
                        }
                    });
                    for (idx, model, label, present) in &k.liveries {
                        ui.horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.label(egui::RichText::new(format!("#{idx}")).monospace().color(crate::theme::dim())).on_hover_text("The position is the index the game saves: never reorder it on a live server");
                            ui.label(model);
                            ui.label(egui::RichText::new(label).small().color(crate::theme::dim()));
                            if *present {
                                ui.label(egui::RichText::new("model file ok").small().color(crate::theme::ok()));
                            } else {
                                ui.label(egui::RichText::new("model file missing in stream/").small().color(crate::theme::warn()));
                            }
                        });
                    }
                }
            }
        }
    }

    fn add_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("Appended after the existing ones, so the indexes already saved on a server do not move. Re-running with the same name changes nothing.");
        form(ui, "livery_add", |ui| {
            row(ui, "Vehicle (model name)", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.vehicle).desired_width(220.0));
            });
            row(ui, "Livery name", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(220.0).hint_text("lspd2"));
            });
            row(ui, "Shown in the mod shop", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.display).desired_width(220.0).hint_text("optional: the name"));
            });
            row(ui, "Label key", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(220.0).hint_text("optional"));
            });
            row(ui, "Kit id", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.kit_id).desired_width(100.0).hint_text("only for a new kit"));
                ui.weak("unique on the whole server");
            });
            row(ui, "Livery model (.yft)", |ui| {
                path_field(ui, &mut self.yft, Pick::OpenFile(&["yft"]));
            });
        });
        if let Some(n) = opt(&self.name).filter(|_| opt(&self.vehicle).is_some()) {
            ui.weak(format!("Streamed as {}_liv_{}.yft", self.vehicle.trim().to_lowercase(), rage_toolkit_core::pack::sanitize(n)));
        }
        let kit_id_ok = self.kit_id.trim().is_empty() || self.kit_id.trim().parse::<u32>().is_ok();
        if !kit_id_ok {
            ui.colored_label(crate::theme::err(), "The kit id must be a whole number.");
        }
        let ready = Path::new(self.pack.trim()).is_dir() && opt(&self.vehicle).is_some() && opt(&self.name).is_some() && kit_id_ok;
        ui.add_space(6.0);
        if ui.add_enabled(ready, big_button("Add livery")).on_disabled_hover_text("Choose the resource folder, the vehicle and a livery name").clicked() {
            let r = livery::add(&AddParams {
                pack: Path::new(self.pack.trim()),
                vehicle: self.vehicle.trim(),
                name: self.name.trim(),
                display: opt(&self.display),
                label: opt(&self.label),
                kit_id: opt(&self.kit_id).and_then(|k| k.parse().ok()),
                yft: opt(&self.yft).map(Path::new),
            });
            self.added = Some(r.map_err(|e| format!("{e:#}")));
            self.reload();
        }
        match &self.added {
            None => {}
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(lines)) => {
                for l in lines {
                    ui.label(egui::RichText::new(l).monospace().color(crate::theme::ok()));
                }
            }
        }
    }

    /// A livery model (.yft) from a .dds or picture, with one of the vehicle's livery files as the shape.
    fn dds_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("Just a .dds to a livery .yft: one of the vehicle's livery files (like polb2_livery1.yft) gives the shape, your picture goes in it. No metas needed. Drop the .yft and the .dds on the window, or:");
        form(ui, "livery_yft", |ui| {
            row(ui, "Template livery (.yft)", |ui| {
                if path_field(ui, &mut self.tpl, Pick::OpenFile(&["yft"])) {
                    let p = PathBuf::from(self.tpl.trim());
                    self.set_template(&p);
                }
            });
            row(ui, "Your livery (.dds)", |ui| {
                path_field(ui, &mut self.pic, Pick::OpenFile(&PICTURES));
            });
            row(ui, "Save as", |ui| {
                path_field(ui, &mut self.save_as, Pick::SaveFile(&["yft"]));
            });
        });
        let ready = Path::new(self.tpl.trim()).is_file() && Path::new(self.pic.trim()).is_file() && opt(&self.save_as).is_some();
        ui.add_space(6.0);
        if ui.add_enabled(ready, big_button("Make the .yft")).on_hover_text("The picture is made the same size and format as the template's texture when it is not already").clicked() {
            let (t, p, o) = (PathBuf::from(self.tpl.trim()), PathBuf::from(self.pic.trim()), PathBuf::from(self.save_as.trim()));
            self.made = Some(rage_toolkit_core::livery_model::livery_yft(&t, &p, &o).map_err(|e| format!("{e:#}")));
            if matches!(self.made, Some(Ok(_))) {
                // ready for Add a livery
                self.yft = o.display().to_string();
            }
        }
        match &self.made {
            Some(Ok(m)) => {
                ui.colored_label(crate::theme::ok(), m);
            }
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            None => {}
        }
    }

    fn check_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("Fails if any existing livery would change index, disappear, or have its kit id changed or reused.");
        form(ui, "livery_check", |ui| {
            row(ui, "Live carcols.meta (old)", |ui| {
                path_field(ui, &mut self.old, Pick::OpenFile(&["meta"]));
            });
            row(ui, "Your carcols.meta (new)", |ui| {
                path_field(ui, &mut self.new, Pick::OpenFile(&["meta"]));
            });
        });
        ui.add_space(6.0);
        if ui.add_enabled(Path::new(self.old.trim()).is_file() && Path::new(self.new.trim()).is_file(), big_button("Check")).clicked() {
            let read = |p: &str| std::fs::read_to_string(p.trim()).map_err(|e| format!("{p}: {e}"));
            self.checked = Some(read(&self.old).and_then(|o| read(&self.new).and_then(|n| livery::append_only(&o, &n).map_err(|e| format!("{e:#}")))));
        }
        match &self.checked {
            None => {}
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(notes)) => {
                ui.colored_label(crate::theme::ok(), "Safe: nothing that exists changes index.");
                for n in notes {
                    ui.label(egui::RichText::new(n).small().color(crate::theme::dim()));
                }
            }
        }
    }
}
