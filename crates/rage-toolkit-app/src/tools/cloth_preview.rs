//! The Clothing page's 3D preview: the selected drawable with the chosen texture, on the bare freemode body of its sex
//! (head, hair, torso, legs, feet from the user's own game, `rage_toolkit_core::clothing::body`). Loading runs on a thread; the
//! drawing is the Viewer's renderer (its own instance and offscreen target) and the Viewer's orbit camera.
use crate::{camera::Camera, gpu::Gpu};
use eframe::{egui, egui_wgpu};
use glam::Vec3;
use rage_toolkit_core::{
    clothing::{body, Kind, Sex},
    game::Game,
    model::{self, LodLevel, MeshPart},
};
use std::{path::PathBuf, sync::mpsc};

/// What to show: the drawable's model and texture file, its sex and component, and whether the body is shown with it.
#[derive(Clone, PartialEq, Debug)]
pub struct Want {
    pub ydd: PathBuf,
    pub texture: Option<PathBuf>,
    pub sex: Sex,
    pub kind: Kind,
    pub body: bool,
}

/// meshes, textures for the GPU (name, width, height, RGBA), bounds, a line for the status
type Scene = (Vec<MeshPart>, Vec<Pixels>, (Vec3, Vec3), String);

pub struct Preview {
    gpu: Option<Gpu>,
    cam: Camera,
    shown: Option<Want>,
    job: Option<(Want, mpsc::Receiver<anyhow::Result<Scene>>)>,
    pub status: String,
    parts: usize,
    bounds: (Vec3, Vec3),
    frame_next: bool,
}

impl Default for Preview {
    fn default() -> Self {
        Self { gpu: None, cam: Camera::new(), shown: None, job: None, status: String::new(), parts: 0, bounds: (Vec3::splat(-1.0), Vec3::splat(1.0)), frame_next: false }
    }
}

/// a texture for the GPU: name, width, height, RGBA
type Pixels = (String, u32, u32, Vec<u8>);

/// Textures of a `.ytd`, or one image (named after the file, as the build names its texture).
fn textures_of(path: &std::path::Path) -> anyhow::Result<Vec<Pixels>> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "ytd" {
        let (ts, _) = rage_toolkit_core::pipeline::read_ytd(path)?;
        Ok(ts.iter().filter_map(|t| model::texture_rgba(t).ok().map(|(w, h, rgba)| (t.name.clone(), w, h, rgba))).collect())
    } else {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let p = rage_toolkit_core::texture::preview_image(&std::fs::read(path)?, &path.to_string_lossy())?;
        Ok(vec![(name, p.width, p.height, p.rgba)])
    }
}

/// Load everything to show (on a thread).
fn load(want: &Want, game: Option<&Game>) -> anyhow::Result<Scene> {
    let mut meshes = model::placed_meshes(&want.ydd, LodLevel::High)?;
    let mut textures = match &want.texture {
        Some(t) => textures_of(t)?,
        None => vec![],
    };
    // the game puts the selected .ytd on the drawable whatever texture name its shaders carry (custom packs often name it
    // anything), so the preview does too; a diffuse inside the .ytd is preferred over a normal or specular map
    let chosen = textures.iter().find(|t| t.0.to_lowercase().contains("diff")).or(textures.first()).map(|t| t.0.clone());
    if let Some(first) = chosen {
        for m in &mut meshes {
            m.diffuse = Some(first.clone());
        }
    }
    let mut note = String::new();
    if want.body {
        match game {
            Some(g) => {
                let replaced = match want.kind {
                    Kind::Component(c) => Some(c),
                    Kind::Prop(_) => None,
                };
                for part in body::freemode_body(g, want.sex)?.into_iter().filter(|p| Some(p.component) != replaced) {
                    meshes.extend(model::placed_meshes(&part.ydd, LodLevel::High)?);
                    if let Some(t) = &part.ytd {
                        textures.extend(textures_of(t)?);
                    }
                }
            }
            None => note = " (no body: GTA V was not found)".into(),
        }
    }
    // the body's textures by their variant (whi / uni)
    let names: Vec<String> = textures.iter().map(|t| t.0.clone()).collect();
    crate::apply_variant_fallback(&mut meshes, &names);
    let (mut mn, mut mx) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for m in &meshes {
        let (a, b) = crate::bounds_of(m);
        mn = mn.min(a);
        mx = mx.max(b);
    }
    if !mn.is_finite() {
        anyhow::bail!("{}: no geometry to show", want.ydd.display());
    }
    let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok((meshes, textures, (mn, mx), format!("{tris} triangles{note}")))
}

impl Preview {
    pub fn loading(&self) -> bool {
        self.job.is_some()
    }

    /// Draw the preview in the space given; starts loading when `want` changed.
    pub fn ui(&mut self, ui: &mut egui::Ui, render: Option<&egui_wgpu::RenderState>, want: Option<Want>, game: Option<&Game>) {
        let Some(rs) = render else {
            ui.weak("The 3D preview needs the GPU renderer.");
            return;
        };
        if self.gpu.is_none() {
            self.gpu = Some(Gpu::new(rs));
        }
        // a new thing to show: load it on a thread (a newer one replaces one still loading)
        if want.is_some() && want != self.shown && self.job.as_ref().map(|j| &j.0) != want.as_ref() {
            let w = want.clone().unwrap();
            let (tx, rx) = mpsc::channel();
            let (g, c, job_want) = (game.cloned(), ui.ctx().clone(), w.clone());
            std::thread::spawn(move || {
                let _ = tx.send(load(&job_want, g.as_ref()));
                c.request_repaint();
            });
            self.job = Some((w, rx));
            self.status = "Loading...".into();
        }
        if let Some((w, rx)) = &self.job {
            match rx.try_recv() {
                Ok(res) => {
                    let w = w.clone();
                    self.job = None;
                    match res {
                        Ok((meshes, textures, bounds, status)) => {
                            let gpu = self.gpu.as_mut().unwrap();
                            gpu.set_meshes(&meshes);
                            gpu.set_textures(&textures);
                            self.parts = meshes.len();
                            self.bounds = bounds;
                            self.status = status;
                            self.frame_next = self.shown.as_ref().map(|s| (s.sex, s.body)) != Some((w.sex, w.body));
                        }
                        Err(e) => self.status = format!("{e:#}"),
                    }
                    self.shown = Some(w);
                }
                Err(mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => self.job = None,
            }
        }
        let size = ui.available_size() - egui::vec2(0.0, 22.0);
        let (rect, resp) = ui.allocate_exact_size(size.max(egui::vec2(60.0, 60.0)), egui::Sense::click_and_drag());
        let resp = resp.on_hover_text("drag: turn   right-drag: move   wheel: zoom   double-click: fit");
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.cam.yaw -= d.x * 0.008;
            self.cam.pitch = (self.cam.pitch + d.y * 0.008).clamp(-1.5, 1.5);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.cam.pan(d.x, d.y);
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.cam.dist = (self.cam.dist * (1.0 - scroll * 0.0015)).clamp(0.05, 50.0);
            }
        }
        let aspect = rect.width() / rect.height().max(1.0);
        if resp.double_clicked() || self.frame_next {
            self.cam.target = (self.bounds.0 + self.bounds.1) * 0.5;
            self.cam.dist = Camera::fit_dist(self.bounds.0, self.bounds.1, aspect);
            // the front of a ped faces -Y: look at it from the front, a little from the side
            self.cam.yaw = -1.25;
            self.cam.pitch = 0.1;
            self.frame_next = false;
        }
        ui.painter().rect_filled(rect, 6.0, crate::theme::surface());
        if self.shown.is_some() && self.parts > 0 {
            let ppp = ui.ctx().pixels_per_point();
            let (w, h) = ((rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
            let gpu = self.gpu.as_mut().unwrap();
            let visible = vec![true; self.parts];
            let id = gpu.render(w, h, self.cam.view_proj(w.max(1) as f32 / h.max(1) as f32), self.cam.eye(), &visible, true, false, [0.6, 0.6, 0.6], false); // matte: the vehicle highlights make skin look like metal
            ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        } else {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, if want_none(&self.shown, &self.job) { "Select a drawable to see it" } else { "Loading..." }, egui::FontId::proportional(14.0), crate::theme::dim());
        }
        ui.weak(&self.status).on_hover_text("drag: turn   right-drag: move   wheel: zoom   double-click: fit");
    }
}

fn want_none(shown: &Option<Want>, job: &Option<(Want, mpsc::Receiver<anyhow::Result<Scene>>)>) -> bool {
    shown.is_none() && job.is_none()
}
