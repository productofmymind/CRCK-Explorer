//! The LOD inspector: how the selected game entity is represented across distances (its parents and children, their levels and
//! distances, whether their models are loaded) and, drawn in the view, the links to them and the distances at which it changes.
use super::*;

const LEVELS: [&str; 7] = ["HD", "LOD", "SLOD 1", "SLOD 2", "SLOD 3", "orphan HD", "SLOD 4"];

fn level_name(l: u8) -> &'static str {
    LEVELS[(l as usize).min(6)]
}

/// a circle of `n` segments as line pairs, in the plane spanned by `a` and `b`
fn circle(c: Vec3, r: f32, a: Vec3, b: Vec3, out: &mut Vec<[f32; 3]>) {
    let n = 48;
    let pt = |i: usize| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        (c + a * (r * t.cos()) + b * (r * t.sin())).to_array()
    };
    for i in 0..n {
        out.push(pt(i));
        out.push(pt(i + 1));
    }
}

impl World {
    /// the one selected entity of the game, with its index
    fn lod_subject(&self) -> Option<(usize, WorldEnt)> {
        let id = *self.sel.first()?;
        if self.sel.len() != 1 || id >= project::BASE {
            return None;
        }
        self.ent_of(id).map(|e| (id as usize, e))
    }

    /// the entity's own LOD distance and the distance at which its children take over, as the view uses them (Detail distance applied)
    fn lod_distances(&self, data: &WorldData, e: &WorldEnt) -> (f32, f32) {
        let own = if e.lod_dist > 0.0 { e.lod_dist } else { data.arch_of(&e.hash).map_or(0.0, |a| a.lod_dist) };
        let child = if e.child_lod_dist < 0.0 { own * 0.5 } else { e.child_lod_dist };
        (own * self.lod_mult, child * self.lod_mult)
    }

    /// Lines for the view: to the parent (cyan), to the children (green), the entity's LOD distance (yellow) and its child distance (orange).
    pub(super) fn lod_overlay_segments(&self) -> [Vec<[f32; 3]>; 4] {
        let mut out: [Vec<[f32; 3]>; 4] = Default::default();
        if !self.show_lod_links {
            return out;
        }
        let (Some((i, e)), Some(data)) = (self.lod_subject(), self.data()) else { return out };
        let c = Vec3::from(e.pos);
        if let Some(p) = data.ents.get(e.parent as usize) {
            out[0].push(c.to_array());
            out[0].push(p.pos);
        }
        for &k in data.children_of(i).iter().take(120) {
            out[1].push(c.to_array());
            out[1].push(data.ents[k as usize].pos);
        }
        let (lod, child) = self.lod_distances(&data, &e);
        for (slot, r) in [(2usize, lod), (3, child)] {
            if r > 0.5 && r < 20000.0 {
                circle(c, r, Vec3::X, Vec3::Y, &mut out[slot]);
                circle(c, r, Vec3::X, Vec3::Z, &mut out[slot]);
            }
        }
        out
    }

    pub(super) fn lod_inspector_section(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("LOD of the selected entity").default_open(false).show(ui, |ui| {
            ui.checkbox(&mut self.show_lod_links, "Draw links and distances").on_hover_text("Cyan: to its parent. Green: to its children. Yellow: its LOD distance. Orange: where its children take over");
            let (Some((i, e)), Some(data)) = (self.lod_subject(), self.data()) else {
                ui.weak("Select one entity of the game to see how it is represented at other distances.");
                return;
            };
            let name = self.name(e.hash);
            let (lod, child) = self.lod_distances(&data, &e);
            let dist = (Vec3::from(e.pos) - self.cam.pos).length();
            ui.label(format!("{name} · {}", level_name(e.lod_level)));
            ui.small(format!("LOD distance {lod:.0} m · children from {child:.0} m · {} children · camera {dist:.0} m away", e.num_children));
            let state = if dist > lod.max(60.0) {
                "not drawn: beyond its LOD distance"
            } else if e.num_children > 0 && dist <= child {
                "stand-in: its children are drawn instead"
            } else {
                "drawn"
            };
            ui.small(format!("now: {state}"));
            let status = |h: u32| -> &'static str {
                match &self.loader {
                    Some(l) if l.get(h).is_some() => "loaded",
                    Some(l) if l.known(h, false) => "no model",
                    Some(_) => "not asked for yet",
                    None => "-",
                }
            };
            ui.small(format!("model: {}", status(e.hash)));
            let mut go: Option<u32> = None;
            // the parents above it
            let mut cur = e;
            let mut depth = 0;
            while let Some(p) = data.ents.get(cur.parent as usize).filter(|_| depth < 6) {
                ui.horizontal(|ui| {
                    ui.small("parent");
                    if ui.link(format!("[{}] {}", level_name(p.lod_level), self.name(p.hash))).clicked() {
                        go = Some(cur.parent);
                    }
                    ui.small(format!("{:.0} m · {}", (Vec3::from(p.pos) - self.cam.pos).length(), status(p.hash)));
                });
                cur = *p;
                depth += 1;
            }
            if depth == 0 {
                ui.small(if e.parent == worldload::NO_PARENT { "no parent: drawn on its own distance" } else { "its parent index points at nothing (see Check LOD links)" });
            }
            let kids = data.children_of(i);
            if !kids.is_empty() {
                ui.small(format!("{} children:", kids.len()));
                egui::ScrollArea::vertical().max_height(160.0).id_salt("lod_kids").show(ui, |ui| {
                    for &k in kids.iter().take(80) {
                        let c = &data.ents[k as usize];
                        ui.horizontal(|ui| {
                            if ui.link(format!("[{}] {}", level_name(c.lod_level), self.name(c.hash))).clicked() {
                                go = Some(k);
                            }
                            ui.small(format!("{:.0} m · {}", (Vec3::from(c.pos) - self.cam.pos).length(), status(c.hash)));
                        });
                    }
                });
            }
            if let Some(id) = go {
                self.select(id, true);
            }
        });
    }
}
