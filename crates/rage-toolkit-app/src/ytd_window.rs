//! A texture dictionary opened from the Explorer: a floating window with every texture as a card (like the classic tools' texture viewer),
//! sizes from small to one-at-a-time, a name filter, and export, replace, rename, delete and add on what is selected.
//! A click selects one texture (Ctrl+click adds), a double-click shows it in its own picture window.
//! Pictures dropped on the window from Explorer are added to the dictionary (a .dds as it is, other pictures encoded).
//! Every edit calls the same core function as the command line (`ytdtools`, `texture`) on a working copy in the temp folder;
//! the file itself changes only on Save (Ctrl+S). The first save of a file keeps a `.bak` beside it. A file from the game
//! is never written: its Save asks where to put the result. Closing a window (or the app) with changes not saved asks first.
use crate::preview::Previews;
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind};
use rage_toolkit_core::{pipeline, texture, ytdtools};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// card width in points; 0 = one texture per row, as wide as the window
const SIZES: [(&str, f32); 5] = [("Small", 72.0), ("Medium", 130.0), ("Large", 200.0), ("Huge", 320.0), ("One", 0.0)];
const CAPTION_H: f32 = 34.0;
/// longest side of a card's picture
const CARD_MAX: u32 = 1024;
/// what can be added to a dictionary
const PICTURES: [&str; 7] = ["dds", "png", "jpg", "jpeg", "tga", "bmp", "webp"];

fn is_picture(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| PICTURES.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

struct Row {
    name: String,
    width: u32,
    height: u32,
    format: String,
    levels: u8,
    bytes: usize,
    thumb: egui::TextureHandle,
}

struct Win {
    id: u64,
    /// the file the window saves to (on disk, or CRCK Explorer's cached copy of a file inside an archive)
    path: PathBuf,
    /// the working copy every edit changes; Save puts it over `path`
    work: PathBuf,
    /// the working copy has changes `path` does not
    dirty: bool,
    /// the window's X was clicked with changes not saved: asking what to do
    asking_close: bool,
    from_game: bool,
    backed_up: bool,
    rows: Vec<Row>,
    selected: BTreeSet<usize>,
    filter: String,
    /// index into SIZES
    size: usize,
    list: bool,
    /// row being renamed, and the text typed so far
    rename: Option<(usize, String)>,
    /// asked "delete these?" and waiting for the answer
    confirm: bool,
    export_png: bool,
    message: String,
    /// the message is an error
    failed: bool,
    /// where the window was drawn last frame: its layer (the top one under the mouse gets a drop) and its rectangle
    layer: Option<egui::LayerId>,
    rect: Option<Rect>,
}

#[derive(Default)]
pub struct Ytds {
    open: Vec<Win>,
    next: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Click {
    Plain,
    Ctrl,
    Double,
    Right,
}

/// What a button asked for; done after the cards are drawn so the list is not changed while it is drawn.
enum Do {
    /// these rows, to a file (one) or a folder (several)
    Export(Vec<usize>),
    Replace(usize),
    Rename(usize, String),
    Delete(Vec<usize>),
    Add,
    Save,
    Revert,
    SaveCopy,
    Preview(usize),
}

/// What the "not saved" question was answered with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    Save,
    Discard,
    Cancel,
}

/// Where the working copy of window `id` lives: the temp folder, never beside the user's file.
fn work_path(id: u64, path: &Path) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("rage-toolkit-work");
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "textures.ytd".into());
    Ok(dir.join(format!("{}-{id}-{name}", std::process::id())))
}

impl Drop for Win {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.work);
    }
}

impl Ytds {
    pub fn open(&mut self, ctx: &egui::Context, path: &Path, from_game: bool) -> Result<(), String> {
        // already open: to the front, with its changes
        if let Some(w) = self.open.iter().find(|w| w.path == path) {
            if let Some(l) = w.layer {
                ctx.move_to_top(l);
            }
            return Ok(());
        }
        self.next += 1;
        let work = work_path(self.next, path).map_err(|e| format!("a working copy: {e}"))?;
        std::fs::copy(path, &work).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut w = Win {
            id: self.next,
            path: path.to_path_buf(),
            work,
            dirty: false,
            asking_close: false,
            from_game,
            backed_up: false,
            rows: vec![],
            selected: BTreeSet::new(),
            filter: String::new(),
            size: 2,
            list: false,
            rename: None,
            confirm: false,
            export_png: false,
            message: String::new(),
            failed: false,
            layer: None,
            rect: None,
        };
        w.reload(ctx).map_err(|e| format!("{}: {e:#}", path.display()))?;
        self.open.push(w);
        Ok(())
    }

    #[allow(deprecated)] // Id::new is the plain way to key a window by a number
    pub fn show(&mut self, ctx: &egui::Context, previews: &mut Previews) {
        // Ctrl+S saves the dictionary window in front
        let top = ctx.top_layer_id();
        if let Some(w) = self.open.iter_mut().find(|w| w.layer.is_some() && w.layer == top) {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                w.save_now();
            }
        }
        self.open.retain_mut(|w| {
            let mut open = true;
            let title = format!("{}{}  ({} texture{})", w.name(), if w.dirty { " *" } else { "" }, w.rows.len(), if w.rows.len() == 1 { "" } else { "s" });
            let shown = egui::Window::new(title).id(egui::Id::new(("ytd", w.id))).open(&mut open).default_size([920.0, 640.0]).resizable(true).show(ctx, |ui| {
                if let Some(d) = w.ui(ui) {
                    w.run(ctx, d, previews);
                }
            });
            (w.layer, w.rect) = match shown {
                Some(r) => (Some(r.response.layer_id), Some(r.response.rect)),
                None => (None, None),
            };
            if !open {
                if !w.dirty {
                    return false;
                }
                w.asking_close = true;
            }
            if w.asking_close {
                match w.close_question(ctx) {
                    Some(Close::Save) => {
                        w.asking_close = false;
                        // closed once saved; a cancelled Save as... or a failed save keeps it open
                        if w.save_now() {
                            return false;
                        }
                    }
                    Some(Close::Discard) => return false,
                    Some(Close::Cancel) => w.asking_close = false,
                    None => {}
                }
            }
            true
        });
    }

    /// The dictionaries with changes not saved, by file name.
    pub fn unsaved(&self) -> Vec<String> {
        self.open.iter().filter(|w| w.dirty).map(|w| w.name()).collect()
    }

    /// Save every dictionary with changes. False when one was not saved (an error, or a Save as... cancelled).
    pub fn save_all(&mut self) -> bool {
        let mut all = true;
        for w in self.open.iter_mut().filter(|w| w.dirty) {
            all &= w.save_now();
        }
        all
    }

    /// Forget every change not saved (the files stay as they were saved last).
    pub fn discard_all(&mut self) {
        for w in &mut self.open {
            w.dirty = false;
        }
    }

    /// The window under `pos` (the top one where windows overlap).
    fn at(&self, ctx: &egui::Context, pos: egui::Pos2) -> Option<usize> {
        let top = ctx.layer_id_at(pos)?;
        self.open.iter().position(|w| w.layer == Some(top) && w.rect.is_some_and(|r| r.contains(pos)))
    }

    /// While files are dragged over the window: the dictionary they would be added to, and the window's rectangle.
    pub fn drop_target(&self, ctx: &egui::Context, pos: Option<egui::Pos2>) -> Option<(String, Rect)> {
        let w = &self.open[self.at(ctx, pos?)?];
        Some((w.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), w.rect?))
    }

    /// Files dropped at `pos`: over a window, its pictures are added to that dictionary. Returns the files left for the
    /// rest of the app (all of them when the drop was not on a window).
    pub fn take_drop(&mut self, ctx: &egui::Context, pos: Option<egui::Pos2>, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let Some(i) = pos.and_then(|p| self.at(ctx, p)) else { return paths };
        let (pictures, rest): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| is_picture(p));
        if !pictures.is_empty() {
            self.open[i].add(ctx, &pictures);
        }
        rest
    }
}

impl Win {
    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    /// Put the working copy over the file. A file from the game is never written: Save as... asks where, and the window
    /// then belongs to that file. Ok(None): the Save as... was cancelled.
    fn save(&mut self) -> anyhow::Result<Option<String>> {
        if self.from_game {
            let Some(out) = rfd::FileDialog::new().set_file_name(self.name()).add_filter("Texture dictionary", &["ytd"]).save_file() else { return Ok(None) };
            std::fs::copy(&self.work, &out)?;
            self.path = out;
            self.from_game = false;
            self.backed_up = true;
        } else {
            if !self.backed_up {
                let bak = PathBuf::from(format!("{}.bak", self.path.display()));
                if !bak.exists() {
                    std::fs::copy(&self.path, &bak)?;
                }
                self.backed_up = true;
            }
            // through a temporary file beside it, so a failed write never leaves half a dictionary
            let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", self.path.display()));
            std::fs::copy(&self.work, &tmp)?;
            if let Err(e) = std::fs::rename(&tmp, &self.path) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.into());
            }
        }
        self.dirty = false;
        Ok(Some(format!("Saved {}", self.path.display())))
    }

    /// Save, with the result as the window's message. True when it was saved.
    fn save_now(&mut self) -> bool {
        match self.save() {
            Ok(Some(m)) => {
                self.message = m;
                self.failed = false;
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.message = format!("Not saved: {e:#}");
                self.failed = true;
                false
            }
        }
    }

    /// The question when the window is closed with changes not saved. None while it is not answered.
    fn close_question(&mut self, ctx: &egui::Context) -> Option<Close> {
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::NULL.with(("ytd_close", self.id))).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(egui::RichText::new("Unsaved changes").strong().size(16.0));
            ui.add_space(4.0);
            ui.label(format!("{} has changes that are not saved. Close it anyway?", self.name()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(if self.from_game { "Save as..." } else { "Save" }).clicked() {
                    choice = Some(Close::Save);
                }
                if ui.button(egui::RichText::new("Close without saving").color(crate::theme::err())).clicked() {
                    choice = Some(Close::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(Close::Cancel);
                }
            });
        });
        // Escape or a click beside it: nothing is lost
        if choice.is_none() && modal.should_close() {
            choice = Some(Close::Cancel);
        }
        choice
    }

    fn reload(&mut self, ctx: &egui::Context) -> anyhow::Result<()> {
        let (textures, _) = pipeline::read_ytd(&self.work)?;
        let mut rows = vec![];
        for (i, t) in textures.iter().enumerate() {
            let (w, h, rgba) = rage_toolkit_core::model::texture_rgba(t).unwrap_or((1, 1, vec![60, 60, 60, 255]));
            // the biggest card is about the window's width: no full-size copy of a 2048 texture (double-click shows that)
            let (w, h, rgba) = crate::gpu::shrink_to(w, h, rgba, CARD_MAX);
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let thumb = ctx.load_texture(format!("ytd{}_{}_{}_{}", self.id, i, t.name, rows.len()), img, egui::TextureOptions::LINEAR);
            rows.push(Row { name: t.name.clone(), width: t.width as u32, height: t.height as u32, format: format!("{:?}", t.format), levels: t.levels, bytes: t.pixel_data.len(), thumb });
        }
        self.rows = rows;
        self.selected.clear();
        self.rename = None;
        self.confirm = false;
        Ok(())
    }

    /// Add pictures as textures named after their files (a texture of the same name is replaced), then show the result.
    fn add(&mut self, ctx: &egui::Context, pictures: &[PathBuf]) {
        let (mut added, mut replaced, mut notes) = (vec![], vec![], vec![]);
        for p in pictures {
            match self.edit(|inp, out| texture::import_image(inp, p, out, None, None)) {
                Ok(r) => {
                    if let Some(w) = r.warning {
                        notes.push(format!("{}: {w}", r.name));
                    }
                    if r.replaced {
                        replaced.push(r.name);
                    } else {
                        added.push(r.name);
                    }
                }
                Err(e) => notes.push(format!("{}: {e:#}", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())),
            }
        }
        let mut msg = vec![];
        if !added.is_empty() {
            msg.push(format!("Added {}", added.join(", ")));
        }
        if !replaced.is_empty() {
            msg.push(format!("Replaced {} (same name)", replaced.join(", ")));
        }
        self.failed = added.is_empty() && replaced.is_empty();
        msg.extend(notes);
        self.message = msg.join(". ");
        if let Err(e) = self.reload(ctx) {
            self.message = format!("{e:#}");
            self.failed = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) -> Option<Do> {
        let mut todo = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Find:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(130.0).hint_text("texture name"));
            ui.separator();
            ui.label("Size:");
            for (i, (name, _)) in SIZES.iter().enumerate() {
                let on = !self.list && self.size == i;
                if ui.selectable_label(on, *name).on_hover_text(if i == 4 { "One texture at a time, as large as the window" } else { "Cards of this size" }).clicked() {
                    self.size = i;
                    self.list = false;
                }
            }
            if ui.selectable_label(self.list, "List").clicked() {
                self.list = true;
            }
            ui.separator();
            ui.weak(format!("{} of {} selected", self.selected.len(), self.rows.len()));
        });
        let place = if self.from_game {
            format!("From the game, which is never written to: Save as... puts your changes in a file you choose.  {}", self.path.display())
        } else {
            format!("{}  (changes are written when you Save; the first save keeps a .bak beside it)", self.path.display())
        };
        ui.weak(format!("{place}   Drop pictures (.dds .png ...) on this window to add them."));
        ui.horizontal_wrapped(|ui| {
            if !self.message.is_empty() {
                ui.label(egui::RichText::new(&self.message).color(if self.failed { crate::theme::err() } else { crate::theme::ok() }));
            }
            if self.dirty {
                ui.label(egui::RichText::new("Not saved yet (Ctrl+S)").color(crate::theme::warn()));
            }
        });
        ui.separator();

        let f = self.filter.trim().to_lowercase();
        let vis: Vec<usize> = (0..self.rows.len()).filter(|&i| f.is_empty() || self.rows[i].name.to_lowercase().contains(&f)).collect();
        // the card clicked, and the right-click menu's choice
        let mut click: Option<(usize, Click)> = None;
        let mut menu: Option<u8> = None;
        let h = (ui.available_height() - 70.0).max(120.0);
        egui::ScrollArea::vertical().id_salt("ytd_cards").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let avail = ui.available_width();
            let mut cards = |ui: &mut egui::Ui| {
                for &i in &vis {
                    let r = &self.rows[i];
                    let size = if self.list {
                        egui::vec2(avail, 30.0)
                    } else if SIZES[self.size].1 == 0.0 {
                        egui::vec2(avail - 4.0, (avail * r.height as f32 / r.width.max(1) as f32).clamp(100.0, 700.0) + CAPTION_H)
                    } else {
                        let w = SIZES[self.size].1;
                        egui::vec2(w, w + CAPTION_H)
                    };
                    let resp = paint_card(ui, r, size, self.selected.contains(&i), self.list);
                    if resp.clicked() {
                        click = Some((i, if ui.input(|i| i.modifiers.command) { Click::Ctrl } else { Click::Plain }));
                    }
                    if resp.secondary_clicked() {
                        click = Some((i, Click::Right));
                    }
                    if resp.double_clicked() {
                        click = Some((i, Click::Double));
                    }
                    resp.context_menu(|ui| {
                        for (n, label) in ["Show larger", "Export...", "Replace...", "Rename", "Delete"].iter().enumerate() {
                            if ui.button(*label).clicked() {
                                menu = Some(n as u8);
                                ui.close();
                            }
                        }
                    });
                }
            };
            if self.list {
                cards(ui);
            } else {
                ui.horizontal_wrapped(|ui| cards(ui));
            }
            if vis.is_empty() {
                ui.weak(if self.rows.is_empty() { "No textures in this dictionary." } else { "No texture matches the filter." });
            }
        });

        if let Some((i, how)) = click {
            match how {
                Click::Ctrl => {
                    if !self.selected.remove(&i) {
                        self.selected.insert(i);
                    }
                }
                // a right click keeps a selection it is part of, so the menu can act on all of it
                Click::Right if self.selected.contains(&i) => {}
                _ => {
                    self.selected.clear();
                    self.selected.insert(i);
                }
            }
            self.confirm = false;
            if how == Click::Double {
                todo = Some(Do::Preview(i));
            }
        }
        let sel: Vec<usize> = self.selected.iter().copied().collect();
        if let Some(m) = menu {
            match (m, sel.as_slice()) {
                (0, [i, ..]) => todo = Some(Do::Preview(*i)),
                (1, s) if !s.is_empty() => todo = Some(Do::Export(sel.clone())),
                (2, [i]) => todo = Some(Do::Replace(*i)),
                (3, [i]) => self.rename = Some((*i, self.rows[*i].name.clone())),
                (4, s) if !s.is_empty() => self.confirm = true,
                _ => {}
            }
        }

        ui.separator();
        if let Some((i, text)) = &mut self.rename {
            let i = *i;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label(format!("Rename {} to:", self.rows[i].name));
                let e = ui.text_edit_singleline(text);
                e.request_focus();
                if ui.button("OK").clicked() || (e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    todo = Some(Do::Rename(i, text.clone()));
                }
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
            if cancel {
                self.rename = None;
            }
        } else if self.confirm && !sel.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("Delete {} texture{} from the dictionary?", sel.len(), if sel.len() == 1 { "" } else { "s" }));
                if ui.button("Yes, delete").clicked() {
                    todo = Some(Do::Delete(sel.clone()));
                }
                if ui.button("No").clicked() {
                    self.confirm = false;
                }
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Export all").on_hover_text("Every texture to a folder (or one file if there is only one)").clicked() {
                    todo = Some(Do::Export((0..self.rows.len()).collect()));
                }
                if ui.add_enabled(!sel.is_empty(), egui::Button::new("Export selected")).clicked() {
                    todo = Some(Do::Export(sel.clone()));
                }
                ui.checkbox(&mut self.export_png, "as PNG").on_hover_text("Folder exports as PNG instead of DDS (a DDS keeps the exact stored pixels)");
                ui.separator();
                if ui.add_enabled(sel.len() == 1, egui::Button::new("Replace...")).on_hover_text("Replace the selected texture with an image or a DDS").clicked() {
                    todo = Some(Do::Replace(sel[0]));
                }
                if ui.add_enabled(sel.len() == 1, egui::Button::new("Rename")).clicked() {
                    self.rename = Some((sel[0], self.rows[sel[0]].name.clone()));
                }
                if ui.add_enabled(!sel.is_empty(), egui::Button::new("Delete")).clicked() {
                    self.confirm = true;
                }
                if ui.button("Add image...").on_hover_text("Add PNG, JPG or DDS files as new textures (named after the files; a DDS goes in as it is). Dropping them on the window does the same.").clicked() {
                    todo = Some(Do::Add);
                }
                ui.separator();
                let save = if self.from_game { "Save as..." } else { "Save" };
                if ui.add_enabled(self.dirty, egui::Button::new(egui::RichText::new(save).strong())).on_hover_text("Ctrl+S").clicked() {
                    todo = Some(Do::Save);
                }
                if ui.add_enabled(self.dirty, egui::Button::new("Revert")).on_hover_text("Forget the changes since the last save").clicked() {
                    todo = Some(Do::Revert);
                }
                if !self.from_game && ui.button("Save a copy as...").on_hover_text("This dictionary as it is now, to another file; the window keeps saving to its own").clicked() {
                    todo = Some(Do::SaveCopy);
                }
            });
        }
        todo
    }

    fn run(&mut self, ctx: &egui::Context, d: Do, previews: &mut Previews) {
        self.message.clear();
        self.failed = false;
        let result: anyhow::Result<String> = match d {
            Do::Preview(i) => {
                let name = self.rows[i].name.clone();
                (|| {
                    let (t, _) = pipeline::read_ytd(&self.work)?;
                    let t = t.iter().find(|t| t.name == name).ok_or_else(|| anyhow::anyhow!("texture is gone"))?;
                    previews.open(ctx, &format!("{name}.dds"), &t.to_dds(), t.pixel_data.len() as u64).map_err(|e| anyhow::anyhow!(e))?;
                    Ok(String::new())
                })()
            }
            Do::Export(idx) => self.export(&idx),
            Do::SaveCopy => match rfd::FileDialog::new().set_file_name(self.name()).add_filter("Texture dictionary", &["ytd"]).save_file() {
                Some(out) => std::fs::copy(&self.work, &out).map(|_| format!("Saved a copy to {}", out.display())).map_err(Into::into),
                None => Ok(String::new()),
            },
            Do::Save => {
                self.save_now();
                return;
            }
            Do::Revert => std::fs::copy(&self.path, &self.work).map(|_| {
                self.dirty = false;
                "Back to the last save".to_string()
            }).map_err(Into::into),
            Do::Replace(i) => {
                let name = self.rows[i].name.clone();
                match rfd::FileDialog::new().add_filter("Image or DDS", &["dds", "png", "jpg", "jpeg", "tga", "bmp", "webp"]).pick_file() {
                    Some(src) => self.edit(|inp, out| texture::replace_texture(inp, &name, &src, out, texture::ReplaceOpts { keep_format: true, keep_size: true }).map(|m| format!("Replaced {name}. {m}"))),
                    None => Ok(String::new()),
                }
            }
            Do::Add => {
                if let Some(files) = rfd::FileDialog::new().add_filter("Image or DDS", &PICTURES).pick_files() {
                    self.add(ctx, &files);
                }
                return;
            }
            Do::Rename(i, new) => {
                let old = self.rows[i].name.clone();
                let new = new.trim().to_string();
                if new.is_empty() || new == old {
                    self.rename = None;
                    Ok(String::new())
                } else {
                    self.edit(|inp, out| ytdtools::rename(inp, out, &[(old.as_str(), new.as_str())]).map(|_| format!("Renamed {old} to {new}")))
                }
            }
            Do::Delete(idx) => {
                let names: Vec<String> = idx.iter().map(|&i| self.rows[i].name.clone()).collect();
                self.edit(|inp, out| {
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    ytdtools::remove(inp, out, &refs).map(|_| format!("Deleted {}", names.join(", ")))
                })
            }
        };
        match result {
            Ok(m) => self.message = m,
            Err(e) => {
                self.message = format!("{e:#}");
                self.failed = true;
            }
        }
        if let Err(e) = self.reload(ctx) {
            self.message = format!("{e:#}");
            self.failed = true;
        }
    }

    /// One texture: a file dialog (DDS or PNG by the name chosen). Several: a folder, one DDS (or PNG) each.
    fn export(&self, idx: &[usize]) -> anyhow::Result<String> {
        if let [i] = idx {
            let name = &self.rows[*i].name;
            return Ok(match rfd::FileDialog::new().set_file_name(format!("{name}.dds")).add_filter("DDS (exact pixels)", &["dds"]).add_filter("PNG", &["png"]).save_file() {
                Some(out) => {
                    texture::export_texture(&self.work, name, &out)?;
                    format!("Exported {name} to {}", out.display())
                }
                None => String::new(),
            });
        }
        let Some(dir) = rfd::FileDialog::new().set_title("Export the textures to this folder").pick_folder() else { return Ok(String::new()) };
        let ext = if self.export_png { "png" } else { "dds" };
        for &i in idx {
            let name = &self.rows[i].name;
            // texture names come from the file: a name with `..` or a drive in it must not write outside the folder
            texture::export_texture(&self.work, name, &rage_toolkit_core::rpf::safe_join(&dir, &format!("{name}.{ext}"))?)?;
        }
        Ok(format!("Exported {} textures to {}", idx.len(), dir.display()))
    }

    /// Run a core edit (input file -> output file) on the working copy; the file itself changes on Save.
    fn edit<T>(&mut self, f: impl FnOnce(&Path, &Path) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let tmp = PathBuf::from(format!("{}.tmp", self.work.display()));
        match f(&self.work, &tmp) {
            Ok(v) => {
                std::fs::rename(&tmp, &self.work)?;
                self.dirty = true;
                Ok(v)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

/// One texture as a card (a thumbnail on a checkerboard with a caption) or, in the list, as a row.
fn paint_card(ui: &mut egui::Ui, r: &Row, size: egui::Vec2, selected: bool, list: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, crate::theme::surface_hi());
    let cap = if list { 0.0 } else { CAPTION_H };
    let area = if list {
        Rect::from_min_size(rect.min + egui::vec2(4.0, 3.0), egui::vec2(24.0, 24.0))
    } else {
        Rect::from_min_max(rect.min + egui::vec2(4.0, 4.0), rect.max - egui::vec2(4.0, 4.0 + cap))
    };
    let k = (area.width() / r.width.max(1) as f32).min(area.height() / r.height.max(1) as f32).min(32.0);
    let img = Rect::from_center_size(area.center(), egui::vec2(r.width as f32 * k, r.height as f32 * k));
    checker(&p, img);
    p.image(r.thumb.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    let info = format!("{}x{}, {}, MipMaps: {}", r.width, r.height, r.format, r.levels);
    if list {
        let y = rect.center().y;
        p.text(egui::pos2(rect.left() + 38.0, y), Align2::LEFT_CENTER, &r.name, FontId::proportional(14.0), crate::theme::text());
        p.text(egui::pos2(rect.right() - 8.0, y), Align2::RIGHT_CENTER, format!("{info}, {}", size_text(r.bytes)), FontId::proportional(12.5), crate::theme::dim());
    } else {
        let strip = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - cap), rect.max);
        p.rect_filled(strip, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 175));
        p.text(strip.left_top() + egui::vec2(6.0, 3.0), Align2::LEFT_TOP, &r.name, FontId::monospace(12.0), Color32::WHITE);
        p.text(strip.left_top() + egui::vec2(6.0, 18.0), Align2::LEFT_TOP, &info, FontId::monospace(11.0), crate::theme::text());
    }
    let teal = crate::theme::info();
    if selected {
        p.rect_stroke(rect, 2.0, Stroke::new(2.0, teal), StrokeKind::Inside);
        let tri = [egui::pos2(rect.right() - 26.0, rect.top()), rect.right_top(), egui::pos2(rect.right(), rect.top() + 26.0)];
        p.add(egui::Shape::convex_polygon(tri.to_vec(), teal, Stroke::NONE));
        p.text(egui::pos2(rect.right() - 8.0, rect.top() + 8.0), Align2::CENTER_CENTER, "✔", FontId::proportional(13.0), Color32::WHITE);
    } else if resp.hovered() {
        p.rect_stroke(rect, 2.0, Stroke::new(1.0, crate::theme::dim()), StrokeKind::Inside);
    }
    resp
}

/// the grey squares behind a texture, so transparency can be seen
fn checker(p: &egui::Painter, rect: Rect) {
    p.rect_filled(rect, 0.0, crate::theme::dim());
    let t = (rect.width().min(rect.height()) / 8.0).clamp(4.0, 16.0);
    let (cols, rows) = ((rect.width() / t).ceil() as i32, (rect.height() / t).ceil() as i32);
    for y in 0..rows.min(200) {
        for x in 0..cols.min(200) {
            if (x + y) % 2 == 1 {
                let tile = Rect::from_min_size(rect.min + egui::vec2(x as f32 * t, y as f32 * t), egui::vec2(t, t)).intersect(rect);
                p.rect_filled(tile, 0.0, crate::theme::dim());
            }
        }
    }
}

fn size_text(b: usize) -> String {
    if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", b / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dds_dropped_on_the_window_is_added_to_its_ytd() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_ytd_drop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("imgs")).unwrap();
        image::RgbaImage::from_fn(64, 64, |x, y| image::Rgba([x as u8 * 4, y as u8 * 4, 40, 255])).save(d.join("imgs/a_d.png")).unwrap();
        let ytd = d.join("mammoth.ytd");
        pipeline::folder_to_ytd(&d.join("imgs"), &ytd, &pipeline::Options::default()).unwrap();
        let dds = d.join("mesh.dds");
        texture::export_texture(&ytd, "a_d", &dds).unwrap();
        let note = d.join("notes.txt");
        std::fs::write(&note, "x").unwrap();

        let ctx = egui::Context::default();
        let (mut ytds, mut previews) = (Ytds::default(), Previews::default());
        ytds.open(&ctx, &ytd, false).unwrap();
        let input = || egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
        // a few frames: a window is measured, then shown
        for _ in 0..3 {
            // no GPU here: the texture uploads a frame asks for are dropped
            ctx.run_ui(input(), |ui| ytds.show(ui.ctx(), &mut previews)).textures_delta.clear();
        }
        let rect = ytds.open[0].rect.expect("the window was drawn");
        assert_eq!(ytds.drop_target(&ctx, Some(rect.center())).map(|t| t.0), Some("mammoth.ytd".to_string()));
        // dropped beside the window: not for it
        let outside = rect.right_bottom() + egui::vec2(20.0, 20.0);
        assert_eq!(ytds.take_drop(&ctx, Some(outside), vec![dds.clone()]), vec![dds.clone()]);
        assert_eq!(ytds.open[0].rows.len(), 1);
        // on the window: the picture is added, anything else is left for the rest of the app
        let rest = ytds.take_drop(&ctx, Some(rect.center()), vec![dds.clone(), note.clone()]);
        assert_eq!(rest, vec![note]);
        let count = |p: &Path| pipeline::read_ytd(p).unwrap().0.len();
        let w = &mut ytds.open[0];
        assert_eq!(w.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["a_d", "mesh"], "{}", w.message);
        assert!(!w.failed && w.message.contains("Added mesh"), "{}", w.message);
        // not written until Save
        assert!(w.dirty && count(&ytd) == 1 && !d.join("mammoth.ytd.bak").exists(), "the file is untouched before Save");
        assert_eq!(ytds.unsaved(), ["mammoth.ytd"]);
        assert!(ytds.save_all());
        assert!(ytds.unsaved().is_empty() && count(&ytd) == 2, "Save writes the file");
        assert_eq!(count(&d.join("mammoth.ytd.bak")), 1, "the first save keeps the file as it was");
        // another change, then Revert: back to the saved file
        let w = &mut ytds.open[0];
        w.edit(|inp, out| ytdtools::remove(inp, out, &["mesh"])).unwrap();
        assert!(w.dirty);
        w.run(&ctx, Do::Revert, &mut previews);
        assert!(!w.dirty && w.rows.len() == 2, "{}", w.message);
        // a change, then closed without saving: the file keeps the last save, the working copy goes
        let work = ytds.open[0].work.clone();
        ytds.open[0].edit(|inp, out| ytdtools::remove(inp, out, &["mesh"])).unwrap();
        ytds.discard_all();
        ytds.open.clear();
        assert!(count(&ytd) == 2 && !work.exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
