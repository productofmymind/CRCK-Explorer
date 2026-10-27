//! Siren settings: check a carcols.meta (`crck-explorer siren check`) and the calculators (`crck-explorer siren calc`).
use super::*;
use rage_toolkit_core::siren;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Calc {
    DegToRad,
    RadToDeg,
    SeqToDec,
    DecToSeq,
    Speed,
    Start,
}

impl Calc {
    const ALL: [Calc; 6] = [Calc::DegToRad, Calc::RadToDeg, Calc::SeqToDec, Calc::DecToSeq, Calc::Speed, Calc::Start];

    fn name(self) -> &'static str {
        match self {
            Calc::DegToRad => "Degrees to radians",
            Calc::RadToDeg => "Radians to degrees",
            Calc::SeqToDec => "Binary pattern to sequencer number",
            Calc::DecToSeq => "Sequencer number to binary pattern",
            Calc::Speed => "Flash speed from BPM",
            Calc::Start => "Start value (flashiness)",
        }
    }

    fn op(self) -> &'static str {
        ["deg2rad", "rad2deg", "seq2dec", "dec2seq", "speed", "start"][self as usize]
    }

    /// (label, starting text) of each input
    fn inputs(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Calc::DegToRad => &[("Degrees", "90")],
            Calc::RadToDeg => &[("Radians", "1.570796")],
            Calc::SeqToDec => &[("Pattern: 32 digits, or 1/2/4/8/16 digits that repeat", "1010")],
            Calc::DecToSeq => &[("Sequencer number", "2863311530")],
            Calc::Speed => &[("BPM", "120"), ("Sequencer: a number, or 0b and a pattern", "0b1010")],
            Calc::Start => &[("Siren number (1 = siren1)", "2"), ("Delta in degrees", "90")],
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Calc::DegToRad | Calc::RadToDeg => "carcols.meta writes angles in radians (float, 6 decimals).",
            Calc::SeqToDec | Calc::DecToSeq => "The sequencer is a 32-beat on/off pattern stored as one number; the first digit is the highest bit.",
            Calc::Speed => "From the community guides' formula (BPM / 100 x pattern multiplier); not verified in game.",
            Calc::Start => "From the guides' recipe; the game's own data does not follow it, so test in game.",
        }
    }
}

pub struct State {
    path: String,
    task: Task<Report>,
    report: Option<Result<Report, String>>,
    filter: IssueFilter,
    calc: Calc,
    inputs: Vec<String>,
    steady: bool,
}

impl Default for State {
    fn default() -> Self {
        let c = Calc::DegToRad;
        Self { path: String::new(), task: Task::default(), report: None, filter: IssueFilter::default(), calc: c, inputs: c.inputs().iter().map(|i| i.1.to_string()).collect(), steady: false }
    }
}

impl State {
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(r) = self.task.poll(ctx) {
            self.report = Some(r);
        }
        ui.columns(2, |cols| {
            card(&mut cols[0], "Check siren settings", |ui| self.check_card(ui));
            card(&mut cols[1], "Calculators", |ui| self.calc_card(ui));
        });
    }

    fn check_card(&mut self, ui: &mut egui::Ui) {
        ui.weak("A carcols.meta (its carvariations.meta next to it is cross-checked), or a whole pack folder.");
        form(ui, "siren_form", |ui| {
            row(ui, "carcols.meta or folder", |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.path).desired_width((ui.available_width() - 190.0).max(120.0)));
                    if ui.button("File...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("carcols.meta", &["meta"]).pick_file() {
                            self.path = p.display().to_string();
                        }
                    }
                    if ui.button("Folder...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            self.path = p.display().to_string();
                        }
                    }
                });
            });
        });
        let p = PathBuf::from(self.path.trim());
        if run_button(ui, "Check", p.exists(), self.task.busy()) {
            self.task.run(move || siren::check(&p));
        }
        match &self.report {
            None => {}
            Some(Err(e)) => {
                ui.colored_label(crate::theme::err(), e);
            }
            Some(Ok(r)) => {
                ui.add_space(6.0);
                issues_view(ui, r, &mut self.filter);
            }
        }
    }

    fn calc_card(&mut self, ui: &mut egui::Ui) {
        let before = self.calc;
        egui::ComboBox::from_id_salt("siren_calc").selected_text(self.calc.name()).width(300.0).show_ui(ui, |ui| {
            for c in Calc::ALL {
                ui.selectable_value(&mut self.calc, c, c.name());
            }
        });
        if before != self.calc {
            self.inputs = self.calc.inputs().iter().map(|i| i.1.to_string()).collect();
        }
        ui.weak(self.calc.hint());
        form(ui, "siren_calc_form", |ui| {
            for (i, (label, _)) in self.calc.inputs().iter().enumerate() {
                row(ui, label, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.inputs[i]).desired_width(260.0));
                });
            }
            if self.calc == Calc::Start {
                row(ui, "Steady corona", |ui| {
                    ui.checkbox(&mut self.steady, "adds 180 degrees");
                });
            }
        });
        let mut args = self.inputs.clone();
        if self.calc == Calc::Start && self.steady {
            args.push("--steady".into());
        }
        ui.add_space(4.0);
        match siren::calc(self.calc.op(), &args) {
            Ok(text) => {
                ui.add(egui::Label::new(egui::RichText::new(text).monospace().color(crate::theme::ok())));
            }
            Err(e) => {
                ui.colored_label(crate::theme::warn(), format!("{e:#}"));
            }
        }
    }
}
