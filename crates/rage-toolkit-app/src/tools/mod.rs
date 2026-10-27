//! The tools that were command line only, in the window: a page per tool in a sidebar, each a small form with its result under it.
//! Every page calls the same `rage-toolkit-core` function the CLI command calls.
use eframe::egui;
use rage_toolkit_core::{game::Game, validate::Report};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::mpsc,
};

mod archives;
mod cloth_preview;
mod clothing;
mod fivem;
mod handling_ed;
mod livery;
mod rename;
mod siren;
mod textures;
mod validate_page;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Pack,
    Validate,
    Handling,
    Livery,
    Textures,
    Clothing,
    Rename,
    Siren,
    Archives,
}

impl Page {
    pub const ALL: [Page; 9] = [Page::Pack, Page::Validate, Page::Handling, Page::Livery, Page::Textures, Page::Clothing, Page::Rename, Page::Siren, Page::Archives];

    /// `--tool NAME` (automated screenshots)
    pub fn from_name(s: &str) -> Option<Page> {
        Some(match s.to_lowercase().as_str() {
            "pack" => Page::Pack,
            "validate" => Page::Validate,
            "handling" => Page::Handling,
            "livery" => Page::Livery,
            "textures" => Page::Textures,
            "rename" => Page::Rename,
            "clothing" => Page::Clothing,
            "siren" => Page::Siren,
            "archives" => Page::Archives,
            _ => return None,
        })
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Pack => "Add-on vehicle pack",
            Page::Validate => "Check a pack",
            Page::Handling => "Handling editor",
            Page::Livery => "Liveries",
            Page::Textures => "Images to YTD",
            Page::Rename => "Batch rename",
            Page::Clothing => "Clothing pack",
            Page::Siren => "Sirens",
            Page::Archives => "Archives (RPF)",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Page::Pack => "Turn your model files and metas into a FiveM resource or a single-player DLC folder, and check the result.",
            Page::Validate => "Cross-check the metas of a pack folder (names that must match, missing files, FiveM size limits) before the game or the server finds the mistake.",
            Page::Handling => "Open a handling.meta, change values with the vanilla range next to them, copy a vanilla vehicle's handling, and save without reformatting the file.",
            Page::Livery => "List the livery kits of a FiveM vehicle resource, add a livery (append-only, safe for a live server), and check an edit against the live file.",
            Page::Textures => "Drop images or folders of them (DDS, PNG, TGA, JPG, ...), choose all into one YTD or each into its own. To look inside a YTD or edit it, double-click it in the Explorer.",
            Page::Rename => "Rename many files at once: numbers, texture letters, drawable numbers, find and replace, patterns. You see every new name and every conflict before anything changes, and can undo.",
            Page::Clothing => "Build a clothing pack for FiveM: drop .ydd models and .ytd textures, they are paired by name and grouped by component; check, preview, and build the resource with its YMT and metas.",
            Page::Siren => "Check the siren settings of a carcols.meta, and the calculators for angles, sequencer patterns and speeds.",
            Page::Archives => "List, extract and build RPF archives (open ones; the game's own archives are in the viewer's Files tab).",
        }
    }
}

pub struct Tools {
    pub page: Page,
    pack: fivem::State,
    validate: validate_page::State,
    handling: handling_ed::State,
    livery: livery::State,
    textures: textures::State,
    rename: rename::State,
    clothing: clothing::State,
    siren: siren::State,
    archives: archives::State,
}

impl Tools {
    pub fn new() -> Self {
        Self {
            page: Page::Pack,
            pack: Default::default(),
            validate: Default::default(),
            handling: Default::default(),
            livery: Default::default(),
            textures: Default::default(),
            rename: Default::default(),
            clothing: Default::default(),
            siren: Default::default(),
            archives: Default::default(),
        }
    }

    /// Files and folders dropped on the window while the Tools are open: the images page takes them.
    pub fn take_drop(&mut self, paths: &[PathBuf]) -> bool {
        match self.page {
            Page::Textures => self.textures.add_dropped(paths),
            Page::Rename => self.rename.add_dropped(paths),
            Page::Clothing => self.clothing.add_dropped(paths),
            Page::Pack => self.pack.take_drop(paths),
            Page::Livery => self.livery.take_drop(paths),
            _ => false,
        }
    }

    /// `--rename-folder DIR [--rename-preset N]` (automated screenshots): the batch rename page with a folder loaded
    pub fn open_rename(&mut self, dir: &Path, preset: Option<usize>) {
        self.page = Page::Rename;
        self.rename.add_dropped(&[dir.to_path_buf()]);
        if let Some(n) = preset {
            self.rename.use_preset(n);
        }
    }

    /// `--clothing-folder DIR` (automated screenshots): the clothing pack page with a folder dropped on it
    pub fn open_clothing(&mut self, dir: &Path) {
        self.page = Page::Clothing;
        self.clothing.open_folder(dir);
    }

    /// `--handling file` (automated screenshots): open the editor on a file
    pub fn open_handling(&mut self, path: &Path) {
        self.page = Page::Handling;
        self.handling.open(path);
    }

    /// `--compare NAME` (automated screenshots): show a vanilla vehicle beside the values
    pub fn compare_handling(&mut self, name: &str) {
        self.handling.compare_with(name);
    }

    /// background work that a screenshot should wait for
    pub fn busy(&self) -> bool {
        self.handling.busy() || self.clothing.busy()
    }

    /// The sidebar: one row per tool.
    pub fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Tools");
        ui.weak("Everything the command line can do.");
        ui.add_space(6.0);
        for p in Page::ALL {
            let r = ui.selectable_label(self.page == p, p.title());
            if r.clicked() {
                self.page = p;
            }
        }
        ui.add_space(10.0);
        ui.separator();
        ui.weak("The game's files are in the Explorer and the 3D view is in the Viewer (top left).");
    }

    /// The page in the middle.
    pub fn page_ui(&mut self, ui: &mut egui::Ui, game: Option<&Game>, render: Option<&eframe::egui_wgpu::RenderState>) {
        let ctx = ui.ctx().clone();
        ui.add_space(2.0);
        ui.heading(self.page.title());
        ui.weak(self.page.blurb());
        ui.separator();
        if self.page == Page::Clothing {
            // its own panels (list, card, 3D preview, actions), not one scrolling column
            self.clothing.ui(ui, &ctx, render, game);
            return;
        }
        egui::ScrollArea::vertical().id_salt(("tool_page", self.page as usize)).auto_shrink([false, false]).show(ui, |ui| match self.page {
            Page::Pack => self.pack.ui(ui, &ctx),
            Page::Validate => self.validate.ui(ui, &ctx),
            Page::Handling => self.handling.ui(ui, &ctx, game),
            Page::Livery => self.livery.ui(ui, &ctx),
            Page::Textures => self.textures.ui(ui, &ctx),
            Page::Rename => self.rename.ui(ui, &ctx),
            Page::Clothing => unreachable!("the clothing page has its own layout"),
            Page::Siren => self.siren.ui(ui, &ctx),
            Page::Archives => self.archives.ui(ui, &ctx),
        });
    }
}

// ---- shared pieces ---------------------------------------------------------------------------------------------

/// What a Browse... button asks for.
#[derive(Clone, Copy)]
pub enum Pick {
    Folder,
    OpenFile(&'static [&'static str]),
    SaveFile(&'static [&'static str]),
}

/// A text field with a Browse... button. True when the dialog chose a new path.
pub fn path_field(ui: &mut egui::Ui, value: &mut String, pick: Pick) -> bool {
    let mut chosen = false;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width((ui.available_width() - 92.0).max(120.0)));
        if ui.button("Browse...").clicked() {
            let start = Path::new(value.as_str()).parent().filter(|p| p.is_dir()).map(Path::to_path_buf);
            let mut d = rfd::FileDialog::new();
            if let Some(s) = &start {
                d = d.set_directory(s);
            }
            let picked = match pick {
                Pick::Folder => d.pick_folder(),
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

/// A two-column form: label on the left, the field on the right.
pub fn form(ui: &mut egui::Ui, id: &str, rows: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([12.0, 8.0]).show(ui, rows);
}

/// One row of a form. A grid gives every widget its own cell, so the field (possibly several widgets) goes in one horizontal cell.
pub fn row(ui: &mut egui::Ui, label: &str, field: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    ui.horizontal(field);
    ui.end_row();
}


/// Work on another thread; the answer is picked up by `poll`. A panic in the work becomes an error message.
pub struct Task<T> {
    rx: Option<mpsc::Receiver<Result<T, String>>>,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { rx: None }
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn run(&mut self, work: impl FnOnce() -> anyhow::Result<T> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = match catch_unwind(AssertUnwindSafe(work)) {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err("internal error: the tool crashed on this input".to_string()),
            };
            let _ = tx.send(r);
        });
        self.rx = Some(rx);
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// The result once it has arrived; keeps the window refreshing while it is pending.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Result<T, String>> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(r) => {
                self.rx = None;
                Some(r)
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                Some(Err("the tool stopped without an answer".to_string()))
            }
        }
    }
}

/// A Run button that is greyed out while work is going on, with a spinner next to it. True when clicked.
/// A rounded card with a title: every Tools page is made of these.
pub fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(crate::theme::surface()).stroke(egui::Stroke::new(1.0, crate::theme::border())).corner_radius(8.0).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).size(16.0).strong());
        ui.add_space(6.0);
        add(ui);
    });
}

/// The main action of a card: big, capitals.
pub fn big_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label.to_uppercase()).strong().size(14.0)).min_size(egui::vec2(180.0, 32.0))
}

/// A dashed-looking box that says what to drop on the window.
pub fn drop_zone(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().stroke(egui::Stroke::new(1.0, crate::theme::border())).corner_radius(8.0).inner_margin(egui::Margin::same(24)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(text).color(crate::theme::dim()).size(15.0));
        });
    });
}

pub fn run_button(ui: &mut egui::Ui, label: &str, enabled: bool, busy: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        clicked = ui.add_enabled(enabled && !busy, big_button(label)).clicked();
        if busy {
            ui.spinner();
        }
    });
    clicked
}

/// The text a finished tool printed, or the error it stopped with.
pub fn output_view(ui: &mut egui::Ui, out: &Option<Result<String, String>>) {
    match out {
        None => {}
        Some(Err(e)) => {
            ui.colored_label(crate::theme::err(), e);
        }
        Some(Ok(text)) => {
            ui.horizontal(|ui| {
                ui.strong("Result");
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(text.clone());
                }
            });
            egui::ScrollArea::both().id_salt("tool_output").max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text.as_str()).monospace()).extend());
            });
        }
    }
}

pub fn sev_colour(sev: char) -> egui::Color32 {
    match sev {
        'E' => crate::theme::err(),
        'W' => crate::theme::warn(),
        _ => crate::theme::info(),
    }
}

/// Which severities an issue list shows.
#[derive(Clone, Copy)]
pub struct IssueFilter {
    pub e: bool,
    pub w: bool,
    pub i: bool,
}

impl Default for IssueFilter {
    fn default() -> Self {
        Self { e: true, w: true, i: true }
    }
}

/// A check's findings: counts, a filter, and one block per issue (what, where, how to fix it).
pub fn issues_view(ui: &mut egui::Ui, r: &Report, f: &mut IssueFilter) {
    let count = |s: char| r.issues.iter().filter(|i| i.sev == s).count();
    let (e, w, i) = (count('E'), count('W'), count('I'));
    ui.horizontal(|ui| {
        if e == 0 && w == 0 {
            ui.colored_label(crate::theme::ok(), "No errors or warnings");
        }
        ui.checkbox(&mut f.e, egui::RichText::new(format!("{e} errors")).color(crate::theme::err()));
        ui.checkbox(&mut f.w, egui::RichText::new(format!("{w} warnings")).color(crate::theme::warn()));
        ui.checkbox(&mut f.i, egui::RichText::new(format!("{i} notes")).color(crate::theme::info()));
        if ui.small_button("Copy").on_hover_text("Copy the list as text").clicked() {
            let text: String = r.issues.iter().map(|x| format!("{} {}{}: {}{}\n", x.sev, x.file, if x.code.is_empty() { String::new() } else { format!(" [{}]", x.code) }, x.msg, if x.fix.is_empty() { String::new() } else { format!(" (fix: {})", x.fix) })).collect();
            ui.ctx().copy_text(format!("{}\n{text}", r.summary));
        }
    });
    if !r.summary.is_empty() {
        ui.weak(&r.summary);
    }
    egui::ScrollArea::vertical().id_salt("issues").max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
        for x in r.issues.iter().filter(|x| match x.sev {
            'E' => f.e,
            'W' => f.w,
            _ => f.i,
        }) {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(x.sev.to_string()).color(sev_colour(x.sev)).strong().monospace());
                ui.label(&x.msg);
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(18.0);
                let mut where_ = x.file.clone();
                if !x.code.is_empty() {
                    where_ = format!("{where_}  [{}]", x.code);
                }
                ui.label(egui::RichText::new(where_).small().color(crate::theme::dim()));
                if !x.fix.is_empty() {
                    ui.label(egui::RichText::new(format!("-> {}", x.fix)).small().color(crate::theme::ok()));
                }
            });
            ui.add_space(2.0);
        }
    });
}

/// Opens a folder (or the folder of a file) in Explorer.
pub fn reveal(path: &Path) {
    let dir: PathBuf = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

pub fn mb(bytes: u64) -> String {
    format!("{:.2} MB", bytes as f64 / 1048576.0)
}

/// One line per texture, for the texture tools' output.
pub fn fmt_reports(rep: &[rage_toolkit_core::pipeline::TextureReport]) -> String {
    let mut s = format!("{:<34} {:>11} {:<8} {:>5} {:>10}  note\n", "texture", "size", "format", "mips", "bytes");
    for t in rep {
        s += &format!("{:<34} {:>11} {:<8} {:>5} {:>10}  {}\n", t.name, format!("{}x{}", t.width, t.height), t.format, t.levels, t.bytes, t.note.as_deref().unwrap_or(""));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_delivers_its_answer_and_survives_a_panic() {
        let ctx = egui::Context::default();
        let mut t: Task<u32> = Task::default();
        assert!(!t.busy() && t.poll(&ctx).is_none());
        t.run(|| Ok(5));
        let got = loop {
            if let Some(r) = t.poll(&ctx) {
                break r;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(got, Ok(5));
        assert!(!t.busy());
        t.run(|| -> anyhow::Result<u32> { panic!("boom") });
        let got = loop {
            if let Some(r) = t.poll(&ctx) {
                break r;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(got.unwrap_err().contains("crashed"));
        t.run(|| anyhow::bail!("no good"));
        let got = loop {
            if let Some(r) = t.poll(&ctx) {
                break r;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(got, Err("no good".to_string()));
    }
}
