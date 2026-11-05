//! Map data files: open a `.ybn` (collision), `.ynd` (path nodes), `.ynv` (navmesh) or a train track text file and edit it with undo. Saving builds
//! the file, reads it back and only then writes it (the first overwrite keeps a `.bak`). The editing itself is in `rage-toolkit-core`
//! (`ybn_doc`, `ynd_doc`, `ynv_doc`, `traintrack`).
use super::*;
use rage_toolkit_core::ymap_doc::Vec3;
use rage_toolkit_core::{traintrack::TrainTrack, ybn_doc::YbnDocument, ynd_doc::YndDocument, ynv_doc::YnvDocument};

enum Doc {
    None,
    Ybn(YbnDocument),
    Ynd(YndDocument),
    Ynv(YnvDocument),
    Train(TrainTrack),
}

pub struct State {
    doc: Doc,
    path: Option<PathBuf>,
    msg: String,
    sel: usize,
    geometry: usize,
    /// text boxes: a triangle's three vertex numbers, a link target, a vertex move
    a: [String; 3],
    from: [f32; 3],
    to: [f32; 3],
}

impl Default for State {
    fn default() -> Self {
        Self { doc: Doc::None, path: None, msg: String::new(), sel: 0, geometry: 0, a: Default::default(), from: [0.0; 3], to: [0.0; 3] }
    }
}

fn write_safely(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if path.exists() {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        if !bak.exists() {
            std::fs::copy(path, &bak)?;
        }
    }
    let tmp = PathBuf::from(format!("{}.crck-tmp", path.display()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn xyz(ui: &mut egui::Ui, v: &mut [f32; 3]) -> bool {
    let mut changed = false;
    for c in v.iter_mut() {
        changed |= ui.add(egui::DragValue::new(c).speed(0.05).max_decimals(3)).changed();
    }
    changed
}

impl State {
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Open file...").on_hover_text(".ybn collision, .ynd path nodes, .ynv navmesh, or a train track text file").clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("Map data", &["ybn", "ynd", "ynv", "dat", "txt"]).pick_file() {
                    self.open(&f);
                }
            }
            let dirty = self.dirty();
            if ui.add_enabled(self.path.is_some() && dirty, egui::Button::new("Save")).on_hover_text("Build the file, read it back, then write it").on_disabled_hover_text("Nothing has changed").clicked() {
                self.save();
            }
            if ui.add_enabled(self.can_undo(), egui::Button::new("Undo")).clicked() {
                self.undo(true);
            }
            if ui.add_enabled(self.can_redo(), egui::Button::new("Redo")).clicked() {
                self.undo(false);
            }
        });
        if let Some(p) = &self.path {
            ui.weak(format!("{}{}", p.display(), if self.dirty() { "  (changed)" } else { "" }));
        }
        if !self.msg.is_empty() {
            ui.label(&self.msg);
        }
        ui.separator();
        match &mut self.doc {
            Doc::None => {
                ui.weak("Open a file to edit it.");
            }
            Doc::Ybn(_) => self.ybn_ui(ui),
            Doc::Ynd(_) => self.ynd_ui(ui),
            Doc::Ynv(_) => self.ynv_ui(ui),
            Doc::Train(_) => self.train_ui(ui),
        }
    }

    fn open(&mut self, f: &Path) {
        self.msg.clear();
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let bytes = match std::fs::read(f) {
            Ok(b) => b,
            Err(e) => {
                self.msg = format!("{}: {e}", f.display());
                return;
            }
        };
        let r: anyhow::Result<Doc> = match ext.as_str() {
            "ybn" => YbnDocument::open(&bytes).map(Doc::Ybn),
            "ynd" => YndDocument::open(&bytes).map(Doc::Ynd),
            "ynv" => YnvDocument::open(&bytes).map(Doc::Ynv),
            _ => Ok(Doc::Train(TrainTrack::parse(&String::from_utf8_lossy(&bytes)))),
        };
        match r {
            Ok(d) => {
                self.doc = d;
                self.path = Some(f.to_path_buf());
                self.sel = 0;
                self.geometry = 0;
            }
            Err(e) => self.msg = format!("{}: {e:#}", f.display()),
        }
    }

    fn dirty(&self) -> bool {
        match &self.doc {
            Doc::None => false,
            Doc::Ybn(d) => d.is_dirty(),
            Doc::Ynd(d) => d.is_dirty(),
            Doc::Ynv(d) => d.is_dirty(),
            Doc::Train(d) => d.is_dirty(),
        }
    }

    fn can_undo(&self) -> bool {
        match &self.doc {
            Doc::Ybn(d) => d.can_undo(),
            Doc::Ynd(d) => d.can_undo(),
            Doc::Ynv(d) => d.is_dirty(),
            Doc::Train(d) => d.is_dirty(),
            Doc::None => false,
        }
    }

    fn can_redo(&self) -> bool {
        match &self.doc {
            Doc::Ybn(d) => d.can_redo(),
            // the other documents report a failed redo by returning false, so the button stays available
            Doc::Ynd(_) | Doc::Ynv(_) | Doc::Train(_) => self.path.is_some(),
            Doc::None => false,
        }
    }

    fn undo(&mut self, undo: bool) {
        let done = match &mut self.doc {
            Doc::Ybn(d) => if undo { d.undo() } else { d.redo() },
            Doc::Ynd(d) => if undo { d.undo() } else { d.redo() },
            Doc::Ynv(d) => if undo { d.undo() } else { d.redo() },
            Doc::Train(d) => if undo { d.undo() } else { d.redo() },
            Doc::None => false,
        };
        if !done {
            self.msg = if undo { "Nothing to undo.".into() } else { "Nothing to redo.".into() };
        }
    }

    fn save(&mut self) {
        let Some(p) = self.path.clone() else { return };
        let built: anyhow::Result<Vec<u8>> = match &self.doc {
            Doc::Ybn(d) => d.to_bytes(),
            Doc::Ynd(d) => d.to_bytes(),
            Doc::Ynv(d) => d.to_bytes(),
            Doc::Train(d) => Ok(d.to_text().into_bytes()),
            Doc::None => return,
        };
        self.msg = match built.and_then(|b| write_safely(&p, &b)) {
            Ok(()) => {
                match &mut self.doc {
                    Doc::Ybn(d) => d.mark_saved(),
                    Doc::Ynd(d) => d.mark_saved(),
                    Doc::Ynv(d) => d.mark_saved(),
                    Doc::Train(d) => d.mark_saved(),
                    Doc::None => {}
                }
                format!("Saved {} (the first version is kept as .bak).", p.display())
            }
            Err(e) => format!("Not saved: {e:#}"),
        };
    }

    // ---- collision ------------------------------------------------------------------------------------------------------------

    fn ybn_ui(&mut self, ui: &mut egui::Ui) {
        let Doc::Ybn(d) = &mut self.doc else { return };
        let n = d.geometry_count();
        if n == 0 {
            ui.weak("This collision file has no polygon geometry.");
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Geometry");
            for g in 0..n.min(16) {
                ui.selectable_value(&mut self.geometry, g, g.to_string());
            }
        });
        let g = self.geometry.min(n - 1);
        let verts = d.vertices(g).unwrap_or_default();
        let polys = d.polygons(g).unwrap_or_default();
        ui.label(format!("{} vertices, {} polygons", verts.len(), polys.len()));
        let mut set: Option<(usize, [f32; 3])> = None;
        let mut kill_poly: Option<usize> = None;
        let mut kill_vert: Option<usize> = None;
        ui.columns(2, |cols| {
            cols[0].strong("Vertices");
            egui::ScrollArea::vertical().id_salt("ybn_v").max_height(320.0).show_rows(&mut cols[0], 26.0, verts.len(), |ui, range| {
                for i in range {
                    ui.horizontal(|ui| {
                        ui.label(format!("{i:>5}"));
                        let mut p = verts[i];
                        if xyz(ui, &mut p) {
                            set = Some((i, p));
                        }
                        if ui.small_button("x").on_hover_text("Remove (refused while a polygon uses it)").clicked() {
                            kill_vert = Some(i);
                        }
                    });
                }
            });
            cols[1].strong("Polygons");
            egui::ScrollArea::vertical().id_salt("ybn_p").max_height(320.0).show_rows(&mut cols[1], 26.0, polys.len(), |ui, range| {
                for i in range {
                    ui.horizontal(|ui| {
                        let vs: Vec<String> = polys[i].vertices().iter().map(|v| v.to_string()).collect();
                        ui.label(format!("{i:>5}  {}  {}", polys[i].kind, vs.join(" ")));
                        if ui.small_button("x").on_hover_text("Remove").clicked() {
                            kill_poly = Some(i);
                        }
                    });
                }
            });
        });
        let mut result: anyhow::Result<()> = Ok(());
        if let Some((i, p)) = set {
            result = d.set_vertex(g, i, p);
        }
        if let Some(i) = kill_poly {
            result = d.remove_polygon(g, i);
        }
        if let Some(i) = kill_vert {
            result = d.remove_vertex(g, i);
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Add vertex").clicked() {
                result = d.add_vertex(g, [0.0, 0.0, 0.0]).map(|_| ());
            }
            ui.label("Triangle of vertices");
            for t in self.a.iter_mut() {
                ui.add(egui::TextEdit::singleline(t).desired_width(40.0));
            }
            if ui.button("Add triangle").clicked() {
                let v: Vec<usize> = self.a.iter().filter_map(|t| t.trim().parse().ok()).collect();
                result = if v.len() == 3 { d.add_triangle(g, [v[0], v[1], v[2]], 0) } else { Err(anyhow::anyhow!("three vertex numbers")) };
            }
        });
        if let Err(e) = result {
            self.msg = format!("{e:#}");
        }
    }

    // ---- path nodes -----------------------------------------------------------------------------------------------------------

    fn ynd_ui(&mut self, ui: &mut egui::Ui) {
        let Doc::Ynd(d) = &mut self.doc else { return };
        let n = d.node_count();
        ui.label(format!("{n} nodes"));
        let mut result: anyhow::Result<()> = Ok(());
        ui.columns(2, |cols| {
            cols[0].strong("Nodes");
            egui::ScrollArea::vertical().id_salt("ynd_n").max_height(360.0).show_rows(&mut cols[0], 22.0, n, |ui, range| {
                for i in range {
                    let p = d.node_position(i).unwrap_or([0.0; 3]);
                    if ui.selectable_label(self.sel == i, format!("{i:>5}   {:.1}, {:.1}, {:.1}", p[0], p[1], p[2])).clicked() {
                        self.sel = i;
                    }
                }
            });
            let ui = &mut cols[1];
            if self.sel < n {
                let i = self.sel;
                let (mut flags, mut street) = {
                    let node = &d.dictionary().nodes[i];
                    ([node.flags0, node.flags1, node.flags2, node.flags3, node.flags4], node.street_name)
                };
                ui.strong(format!("Node {i}"));
                let mut p = d.node_position(i).unwrap_or([0.0; 3]);
                ui.horizontal(|ui| {
                    ui.label("Position");
                    if xyz(ui, &mut p) {
                        result = d.set_position(i, p);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Flags");
                    let mut ch = false;
                    for f in flags.iter_mut() {
                        ch |= ui.add(egui::DragValue::new(f).range(0..=255)).changed();
                    }
                    if ch {
                        result = d.set_flags(i, flags);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Street name (text hash)");
                    if ui.add(egui::DragValue::new(&mut street).hexadecimal(8, false, true)).changed() {
                        result = d.set_street_name(i, street);
                    }
                });
                ui.separator();
                ui.strong("Links");
                for (k, l) in d.links_of(i).iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("to cell {} node {}  flags {} {} {}", l.area_id, l.node_id, l.flags0, l.flags1, l.flags2));
                        if ui.small_button("x").clicked() {
                            result = d.remove_link(i, k);
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label("Link to node");
                    ui.add(egui::TextEdit::singleline(&mut self.a[0]).desired_width(60.0));
                    if ui.button("Add link").clicked() {
                        result = match self.a[0].trim().parse::<usize>() {
                            Ok(to) => d.add_link(i, to, [0, 0, 0]).map(|_| ()),
                            Err(_) => Err(anyhow::anyhow!("a node number")),
                        };
                    }
                });
                if ui.button(egui::RichText::new("Remove this node").color(crate::theme::warn())).on_hover_text("The links that point at it go too; later nodes are renumbered").clicked() {
                    result = d.remove_node(i);
                }
            }
        });
        if ui.button("Add node").clicked() {
            let at = d.node_position(self.sel).unwrap_or([0.0; 3]);
            match d.add_node([at[0] + 5.0, at[1], at[2]]) {
                Ok(i) => self.sel = i,
                Err(e) => result = Err(e),
            }
        }
        if let Err(e) = result {
            self.msg = format!("{e:#}");
        }
    }

    // ---- navmesh --------------------------------------------------------------------------------------------------------------

    fn ynv_ui(&mut self, ui: &mut egui::Ui) {
        let Doc::Ynv(d) = &mut self.doc else { return };
        let n = d.poly_count();
        ui.label(format!("{n} polygons"));
        let mut result: anyhow::Result<()> = Ok(());
        ui.columns(2, |cols| {
            cols[0].strong("Polygons");
            egui::ScrollArea::vertical().id_salt("ynv_p").max_height(360.0).show_rows(&mut cols[0], 22.0, n, |ui, range| {
                for i in range {
                    let p = &d.mesh().polys[i];
                    if ui.selectable_label(self.sel == i, format!("{i:>5}   {} corners", p.vertices.len())).clicked() {
                        self.sel = i;
                    }
                }
            });
            let ui = &mut cols[1];
            if self.sel < n {
                let i = self.sel;
                let (mut f0, mut f1, mut f2, corners) = {
                    let p = &d.mesh().polys[i];
                    (p.flags0 as u32, p.flags1, p.flags2, p.vertices.clone())
                };
                ui.strong(format!("Polygon {i}"));
                let mut ch = false;
                egui::Grid::new("ynv_flags").num_columns(2).show(ui, |ui| {
                    ui.label("Flags 0");
                    ch |= ui.add(egui::DragValue::new(&mut f0).range(0..=65535).hexadecimal(4, false, true)).changed();
                    ui.end_row();
                    ui.label("Flags 1");
                    ch |= ui.add(egui::DragValue::new(&mut f1).hexadecimal(8, false, true)).changed();
                    ui.end_row();
                    ui.label("Flags 2");
                    ch |= ui.add(egui::DragValue::new(&mut f2).hexadecimal(8, false, true)).changed();
                    ui.end_row();
                });
                if ch {
                    result = d.set_flags(i, f0 as u16, f1, f2);
                }
                for (k, v) in corners.iter().enumerate() {
                    ui.label(format!("corner {k}: {:.2}, {:.2}, {:.2}", v.x, v.y, v.z));
                }
                if ui.button(egui::RichText::new("Remove polygon").color(crate::theme::warn())).clicked() {
                    result = d.remove_poly(i);
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Move every corner at");
            xyz(ui, &mut self.from);
            ui.label("to");
            xyz(ui, &mut self.to);
            if ui.button("Move").on_hover_text("Polygons that share the corner stay joined").clicked() {
                result = d.move_vertex(Vec3::new(self.from[0], self.from[1], self.from[2]), Vec3::new(self.to[0], self.to[1], self.to[2])).and_then(|c| if c == 0 { Err(anyhow::anyhow!("no corner is there")) } else { Ok(()) });
            }
        });
        if ui.button("Join edges").on_hover_text("Join polygons that share an edge (within 5 cm)").clicked() {
            result = d.join_edges();
        }
        if let Err(e) = result {
            self.msg = format!("{e:#}");
        }
    }

    // ---- train track ----------------------------------------------------------------------------------------------------------

    fn train_ui(&mut self, ui: &mut egui::Ui) {
        let Doc::Train(d) = &mut self.doc else { return };
        let n = d.node_count();
        ui.label(format!("{n} nodes, {:.0} m long", d.length()));
        let mut result: anyhow::Result<()> = Ok(());
        egui::ScrollArea::vertical().id_salt("train").max_height(380.0).show_rows(ui, 26.0, n, |ui, range| {
            for i in range {
                let Some((mut p, rest)) = d.node(i) else { continue };
                ui.horizontal(|ui| {
                    ui.label(format!("{i:>5}"));
                    if xyz(ui, &mut p) {
                        result = d.move_node(i, p);
                    }
                    ui.weak(rest);
                    if ui.small_button("+").on_hover_text("Insert a node after this one").clicked() {
                        result = d.insert_node(Some(i), [p[0] + 5.0, p[1], p[2]], None).map(|_| ());
                    }
                    if ui.small_button("x").on_hover_text("Remove").clicked() {
                        result = d.remove_node(i);
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Reverse").on_hover_text("Run the track the other way").clicked() {
                d.reverse();
            }
            if ui.button("Add node at the end").clicked() {
                let last = n.checked_sub(1).and_then(|k| d.node(k)).map_or([0.0; 3], |x| x.0);
                result = d.insert_node(n.checked_sub(1), [last[0] + 5.0, last[1], last[2]], None).map(|_| ());
            }
        });
        ui.weak("Lines that are not nodes (comments, headers) and the text of untouched nodes are written back exactly as they were.");
        if let Err(e) = result {
            self.msg = format!("{e:#}");
        }
    }
}
