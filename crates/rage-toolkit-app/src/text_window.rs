//! A text file (.meta, .xml, .lua, ...) opened from the Explorer: a floating editor with Save, Save as and Revert, and, for XML, the
//! metadata validator (`rage_toolkit_core::metaxml`): red / amber underlines in the text, a list of every finding with its line, column and
//! how to fix it (click one to jump there), live while typing (after a short pause) and on the Validate button.
//! A file on disk is saved in place (the first save keeps a `.bak` beside it); a file from the game is a cached copy, so only
//! "Save as..." is offered, and the game is never written to. Line endings and a leading byte-order mark are kept as they were.
use eframe::egui::{self, text::CCursor, text_selection::CCursorRange, Color32, Pos2, Stroke};
use rage_toolkit_core::metaxml::{self, Diagnostic, Report, Severity};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// bigger files make the editor crawl; they can still be extracted
const MAX_BYTES: usize = 1_500_000;
/// text up to this size is checked on every keystroke; larger text waits for a pause
const INSTANT_BYTES: usize = 60_000;
const PAUSE: Duration = Duration::from_millis(300);

struct Win {
    id: u64,
    path: PathBuf,
    from_game: bool,
    backed_up: bool,
    text: String,
    /// the text as last read or saved
    saved: String,
    crlf: bool,
    bom: bool,
    message: String,
    xml: bool,
    live: bool,
    show_notes: bool,
    report: Report,
    /// char index where each line of `text` starts, for the report's line / column
    line_starts: Vec<usize>,
    /// the text changed at this moment and has not been checked since
    pending: Option<Instant>,
    /// char index to move the cursor to and scroll into view
    jump: Option<usize>,
}

/// Something the window wants the application to do.
pub enum Request {
    /// open this file in the Tools > Handling editor
    Handling(PathBuf),
}

#[derive(Default)]
pub struct Texts {
    open: Vec<Win>,
    next: u64,
}

impl Texts {
    pub fn open(&mut self, path: &Path, from_game: bool) -> Result<(), String> {
        if self.open.iter().any(|w| w.path == path) {
            return Ok(());
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if bytes.len() > MAX_BYTES {
            return Err(format!("{}: {:.1} MB is too big for the text editor (right-click > Extract...)", path.display(), bytes.len() as f64 / 1e6));
        }
        if bytes.contains(&0) {
            return Err(format!("{}: not a text file", path.display()));
        }
        let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let body = if bom { &bytes[3..] } else { &bytes[..] };
        let text = String::from_utf8(body.to_vec()).map_err(|_| format!("{}: not UTF-8 text", path.display()))?;
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text };
        let lower = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let xml = lower.ends_with(".meta") || lower.ends_with(".xml");
        self.next += 1;
        let mut w = Win { id: self.next, path: path.to_path_buf(), from_game, backed_up: false, saved: text.clone(), text, crlf, bom, message: String::new(), xml, live: true, show_notes: false, report: Report::default(), line_starts: vec![0], pending: None, jump: None };
        w.validate();
        self.open.push(w);
        Ok(())
    }

    #[allow(deprecated)] // Id::new is the plain way to key a window by a number
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<Request> {
        let mut reqs = vec![];
        self.open.retain_mut(|w| {
            let mut open = true;
            let name = w.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let title = format!("{name}{}", if w.dirty() { "  *" } else { "" });
            egui::Window::new(title).id(egui::Id::new(("text", w.id))).open(&mut open).default_size([820.0, 600.0]).resizable(true).show(ctx, |ui| w.ui(ui, &name, &mut reqs));
            open
        });
        reqs
    }
}

fn colour(s: Severity) -> Color32 {
    match s {
        Severity::Error => crate::theme::err(),
        Severity::Warning => crate::theme::warn(),
        Severity::Info => crate::theme::info(),
    }
}

impl Win {
    fn dirty(&self) -> bool {
        self.text != self.saved
    }

    /// run the validator now
    fn validate(&mut self) {
        self.pending = None;
        if !self.xml {
            return;
        }
        self.report = metaxml::check(&self.text);
        self.line_starts = std::iter::once(0).chain(self.text.chars().enumerate().filter(|(_, c)| *c == '\n').map(|(i, _)| i + 1)).collect();
    }

    fn char_index(&self, d: &Diagnostic) -> usize {
        self.line_starts.get(d.line.saturating_sub(1)).copied().unwrap_or(0) + d.col.saturating_sub(1)
    }

    #[allow(deprecated)] // Id::new keys the editor by the window number
    fn ui(&mut self, ui: &mut egui::Ui, name: &str, reqs: &mut Vec<Request>) {
        let lower = name.to_lowercase();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.dirty() && !self.from_game, egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked() {
                self.save_in_place();
            }
            if ui.button("Save as...").clicked() {
                if let Some(out) = rfd::FileDialog::new().set_file_name(name).save_file() {
                    self.write_to(&out);
                }
            }
            if ui.add_enabled(self.dirty(), egui::Button::new("Revert")).clicked() {
                self.text = self.saved.clone();
                self.validate();
                self.message.clear();
            }
            if self.xml {
                ui.separator();
                if ui.button("Validate").on_hover_text("Check the XML and the game's rules for this file now").clicked() {
                    self.validate();
                }
                ui.checkbox(&mut self.live, "Live").on_hover_text("Check while typing, a moment after you stop");
                ui.checkbox(&mut self.show_notes, "Notes").on_hover_text("Also show information-level findings (names the reference tool's tables do not know, ...)");
            }
            if lower == "handling.meta" && ui.add_enabled(!self.dirty(), egui::Button::new("Open in the handling editor")).on_hover_text("Values with the vanilla range beside them (save first)").clicked() {
                reqs.push(Request::Handling(self.path.clone()));
            }
        });
        if self.from_game {
            ui.weak("From the game: this is a cached copy. Use Save as... to keep your changes.");
        } else {
            ui.weak(format!("{}  (the first save keeps a .bak beside it)", self.path.display()));
        }
        if !self.message.is_empty() {
            ui.label(egui::RichText::new(&self.message).color(crate::theme::ok()));
        }
        if self.xml {
            self.summary(ui);
        }
        ui.separator();

        // the editor, with the findings underlined
        let edit_id = egui::Id::new(("text_edit", self.id));
        let mut edited = false;
        let mut save_key = false;
        let findings: Vec<(usize, usize, Severity)> = if self.xml { self.report.diagnostics.iter().filter(|d| self.show_notes || d.severity != Severity::Info).map(|d| (self.char_index(d), d.len, d.severity)).collect() } else { vec![] };
        let panel_h = if self.xml && !self.report.diagnostics.is_empty() { 150.0 } else { 0.0 };
        let editor_h = (ui.available_height() - panel_h - 14.0).max(120.0);
        let jump = self.jump.take();
        egui::ScrollArea::both().id_salt("text_body").max_height(editor_h).auto_shrink([false, false]).show(ui, |ui| {
            let out = egui::TextEdit::multiline(&mut self.text).id(edit_id).font(egui::TextStyle::Monospace).code_editor().desired_width(f32::INFINITY).desired_rows(20).lock_focus(true).show(ui);
            edited = out.response.changed();
            save_key = out.response.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
            let painter = ui.painter_at(ui.clip_rect());
            for (start, len, sev) in &findings {
                let a = out.galley.pos_from_cursor(CCursor::new(*start));
                let b = out.galley.pos_from_cursor(CCursor::new(*start + (*len).max(1)));
                // one row only: when the end lands on another row, underline to the end of the first
                let x1 = if (b.min.y - a.min.y).abs() < 1.0 { b.min.x } else { a.min.x + (*len as f32).min(40.0) * 7.0 };
                squiggle(&painter, out.galley_pos + a.left_bottom().to_vec2(), (x1 - a.min.x).max(6.0), colour(*sev));
            }
            if let Some(idx) = jump {
                let r = out.galley.pos_from_cursor(CCursor::new(idx));
                ui.scroll_to_rect(r.translate(out.galley_pos.to_vec2()).expand(40.0), Some(egui::Align::Center));
            }
        });
        if let Some(idx) = jump {
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(idx))));
                st.store(ui.ctx(), edit_id);
            }
            ui.ctx().memory_mut(|m| m.request_focus(edit_id));
        }
        if self.xml && !self.report.diagnostics.is_empty() {
            ui.add_space(4.0);
            self.findings_panel(ui);
        }

        if edited {
            self.xml_changed(ui.ctx());
        }
        if let Some(since) = self.pending {
            if since.elapsed() >= PAUSE {
                self.validate();
            } else {
                ui.ctx().request_repaint_after(PAUSE - since.elapsed());
            }
        }
        if save_key && self.dirty() && !self.from_game {
            self.save_in_place();
        }
    }

    /// the text changed: check it now when it is small, after a pause when it is large
    fn xml_changed(&mut self, _ctx: &egui::Context) {
        if !self.xml || !self.live {
            return;
        }
        if self.text.len() <= INSTANT_BYTES {
            self.validate();
        } else {
            self.pending = Some(Instant::now());
        }
    }

    fn summary(&self, ui: &mut egui::Ui) {
        let r = &self.report;
        let (e, w) = (r.errors(), r.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count());
        let kind = match (&r.root, r.structure_checked) {
            (Some(root), true) => format!("  Checked as {root}"),
            (Some(root), false) => format!("  Root <{root}>: syntax checked only (no rules for this structure)"),
            _ => String::new(),
        };
        let stale = if self.pending.is_some() || (!self.live && self.dirty()) { "  (changed since the last check)" } else { "" };
        if e == 0 && w == 0 {
            ui.label(egui::RichText::new(format!("No errors or warnings.{kind}{stale}")).color(crate::theme::ok()));
        } else {
            let col = if e > 0 { colour(Severity::Error) } else { colour(Severity::Warning) };
            ui.label(egui::RichText::new(format!("{e} error{}, {w} warning{}.{kind}{stale}", if e == 1 { "" } else { "s" }, if w == 1 { "" } else { "s" })).color(col));
        }
    }

    fn findings_panel(&mut self, ui: &mut egui::Ui) {
        let mut go = None;
        egui::ScrollArea::vertical().id_salt("text_findings").max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
            for d in self.report.diagnostics.iter().filter(|d| self.show_notes || d.severity != Severity::Info) {
                let tag = match d.severity {
                    Severity::Error => "ERROR",
                    Severity::Warning => "WARN ",
                    Severity::Info => "NOTE ",
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(tag).monospace().color(colour(d.severity)));
                    if ui.link(format!("line {}, col {}", d.line, d.col)).on_hover_text("Go to it").clicked() {
                        go = Some(self.char_index(d));
                    }
                    ui.label(&d.message);
                });
                if !d.fix.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(52.0);
                        ui.weak(format!("How to fix: {}", d.fix));
                    });
                }
            }
        });
        if go.is_some() {
            self.jump = go;
        }
    }

    /// the bytes to write: line endings and byte-order mark as they were read
    fn bytes(&self) -> Vec<u8> {
        let body = if self.crlf { self.text.replace('\n', "\r\n") } else { self.text.clone() };
        let mut out = if self.bom { vec![0xEF, 0xBB, 0xBF] } else { vec![] };
        out.extend_from_slice(body.as_bytes());
        out
    }

    fn save_in_place(&mut self) {
        let path = self.path.clone();
        if !self.from_game && !self.backed_up {
            let bak = PathBuf::from(format!("{}.bak", path.display()));
            if !bak.exists() {
                if let Err(e) = std::fs::copy(&path, &bak) {
                    self.message = format!("could not keep a .bak: {e}");
                    return;
                }
            }
            self.backed_up = true;
        }
        self.write_to(&path);
    }

    /// write to a temp file next to the target, then move it over
    fn write_to(&mut self, out: &Path) {
        let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", out.display()));
        let r = std::fs::write(&tmp, self.bytes()).and_then(|_| std::fs::rename(&tmp, out));
        match r {
            Ok(()) => {
                self.saved = self.text.clone();
                self.message = format!("Saved {}", out.display());
                if out != self.path {
                    self.path = out.to_path_buf();
                    self.from_game = false;
                    self.backed_up = true;
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                self.message = format!("could not save: {e}");
            }
        }
    }
}

/// a wavy line starting at `from`, `width` long
fn squiggle(painter: &egui::Painter, from: Pos2, width: f32, col: Color32) {
    let step = 3.0;
    let n = (width / step).ceil() as usize + 1;
    let pts: Vec<Pos2> = (0..n).map(|i| Pos2::new(from.x + i as f32 * step, from.y + if i % 2 == 0 { 0.0 } else { 2.0 })).collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.3, col)));
}
