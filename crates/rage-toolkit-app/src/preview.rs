//! Picture previews: a `.dds` (or png/jpg/...) opened from the Explorer shows in a floating window, so the Explorer stays where it is.
use eframe::egui;

struct Preview {
    id: u64,
    title: String,
    tex: egui::TextureHandle,
    size: [usize; 2],
    info: String,
    /// show at 100 % instead of fitted to the window
    actual: bool,
}

#[derive(Default)]
pub struct Previews {
    open: Vec<Preview>,
    next: u64,
}

impl Previews {
    /// Decode `bytes` and open a window for it. Returns the error text when the file cannot be read.
    pub fn open(&mut self, ctx: &egui::Context, name: &str, bytes: &[u8], file_size: u64) -> Result<(), String> {
        let p = rage_toolkit_core::texture::preview_image(bytes, name).map_err(|e| format!("{name}: {e:#}"))?;
        let size = [p.width as usize, p.height as usize];
        self.next += 1;
        let tex = ctx.load_texture(format!("preview_{}", self.next), egui::ColorImage::from_rgba_unmultiplied(size, &p.rgba), egui::TextureOptions::LINEAR);
        let info = format!("{}x{}  {}  {}", p.width, p.height, p.info, human(file_size));
        self.open.push(Preview { id: self.next, title: name.to_string(), tex, size, info, actual: false });
        Ok(())
    }

    #[allow(deprecated)] // Id::new is the plain way to key a window by a number
    pub fn show(&mut self, ctx: &egui::Context) {
        self.open.retain_mut(|p| {
            let mut open = true;
            egui::Window::new(&p.title).id(egui::Id::new(("preview", p.id))).open(&mut open).default_size([520.0, 480.0]).resizable(true).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.weak(&p.info);
                    ui.checkbox(&mut p.actual, "100%");
                });
                ui.separator();
                let full = egui::vec2(p.size[0] as f32, p.size[1] as f32);
                if p.actual {
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(egui::Image::new((p.tex.id(), full)));
                    });
                } else {
                    let room = ui.available_size();
                    let k = (room.x / full.x).min(room.y / full.y).max(0.01);
                    ui.centered_and_justified(|ui| {
                        ui.add(egui::Image::new((p.tex.id(), full * k.min(8.0))));
                    });
                }
            });
            open
        });
    }
}

fn human(b: u64) -> String {
    if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{:.0} KB", b as f64 / 1024.0)
    }
}
