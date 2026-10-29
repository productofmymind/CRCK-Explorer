//! Design tokens and themes. Every colour the interface uses comes from one `Tokens` set (the current theme); `apply` turns it into
//! egui's visuals, so menus, dialogs, popups and every window follow it, and the helpers (`text()`, `dim()`, `ok()` ...) give the
//! same colours to code that paints by hand. Custom themes are small text files (`name`, then `key=#rrggbb` lines) in the themes
//! folder, which is also the import / export format.
use eframe::egui::{self, Color32, CornerRadius, Stroke};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tokens {
    /// the page behind everything, and the background of text boxes
    pub bg: Color32,
    /// panels, cards, windows, menus
    pub surface: Color32,
    /// buttons, rows under the mouse, tiles
    pub surface_hi: Color32,
    pub border: Color32,
    pub text: Color32,
    /// secondary text
    pub dim: Color32,
    /// selection, links, the highlight of the current tab
    pub accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub error: Color32,
    pub info: Color32,
}

const fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const DARK_STATUS: [Color32; 4] = [c(140, 210, 140), c(255, 200, 90), c(255, 130, 110), c(110, 170, 240)];
const LIGHT_STATUS: [Color32; 4] = [c(30, 130, 60), c(176, 110, 0), c(200, 50, 40), c(30, 90, 190)];

impl Tokens {
    const fn new(bg: Color32, surface: Color32, surface_hi: Color32, border: Color32, text: Color32, dim: Color32, accent: Color32, status: [Color32; 4]) -> Self {
        Self { bg, surface, surface_hi, border, text, dim, accent, ok: status[0], warn: status[1], error: status[2], info: status[3] }
    }

    /// whether the page is dark (decides egui's base visuals)
    pub fn is_dark(&self) -> bool {
        let l = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
        l(self.bg) < 128.0
    }

    /// `(label, colour)` of every token, for the editor
    pub fn fields(&mut self) -> [(&'static str, &'static str, &mut Color32); 11] {
        [
            ("bg", "Background", &mut self.bg),
            ("surface", "Panels and cards", &mut self.surface),
            ("surface_hi", "Buttons and hover", &mut self.surface_hi),
            ("border", "Borders", &mut self.border),
            ("text", "Text", &mut self.text),
            ("dim", "Secondary text", &mut self.dim),
            ("accent", "Accent", &mut self.accent),
            ("ok", "Success", &mut self.ok),
            ("warn", "Warning", &mut self.warn),
            ("error", "Error", &mut self.error),
            ("info", "Information", &mut self.info),
        ]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
}

pub const AMOLED: Tokens = Tokens::new(c(0, 0, 0), c(10, 10, 12), c(24, 24, 29), c(38, 38, 46), c(236, 236, 242), c(138, 138, 150), c(122, 167, 255), DARK_STATUS);
pub const GRAPHITE: Tokens = Tokens::new(c(27, 28, 31), c(36, 38, 42), c(48, 51, 57), c(62, 66, 74), c(230, 232, 236), c(144, 151, 161), c(110, 168, 254), DARK_STATUS);
pub const LIGHT: Tokens = Tokens::new(c(243, 244, 246), c(255, 255, 255), c(236, 238, 242), c(213, 216, 222), c(27, 29, 34), c(102, 108, 120), c(37, 99, 235), LIGHT_STATUS);
pub const DARK_BLUE: Tokens = Tokens::new(c(12, 20, 36), c(18, 28, 48), c(27, 40, 68), c(40, 58, 94), c(228, 235, 248), c(138, 154, 184), c(77, 163, 255), DARK_STATUS);
pub const LIGHT_GREEN: Tokens = Tokens::new(c(238, 245, 238), c(251, 254, 251), c(225, 238, 226), c(196, 216, 198), c(26, 42, 28), c(93, 116, 97), c(46, 158, 79), LIGHT_STATUS);
pub const SOFT_WHITE: Tokens = Tokens::new(c(232, 230, 225), c(244, 242, 237), c(222, 219, 212), c(200, 197, 189), c(38, 36, 33), c(110, 106, 98), c(52, 104, 196), LIGHT_STATUS);
pub const SILVER: Tokens = Tokens::new(c(206, 209, 214), c(222, 225, 230), c(190, 194, 201), c(165, 170, 179), c(28, 30, 35), c(80, 86, 96), c(40, 90, 180), LIGHT_STATUS);
pub const MID_GREY: Tokens = Tokens::new(c(84, 87, 94), c(96, 100, 108), c(112, 117, 126), c(132, 137, 147), c(240, 241, 244), c(190, 195, 204), c(140, 190, 255), DARK_STATUS);
pub const PASTEL_PINK: Tokens = Tokens::new(c(250, 232, 238), c(255, 244, 247), c(246, 218, 228), c(232, 192, 207), c(74, 38, 52), c(143, 94, 112), c(214, 82, 130), LIGHT_STATUS);

pub fn builtin() -> Vec<Theme> {
    [("AMOLED Black", AMOLED), ("Graphite Grey", GRAPHITE), ("White / Light", LIGHT), ("Dark Blue", DARK_BLUE), ("Light Green", LIGHT_GREEN), ("Soft White", SOFT_WHITE), ("Silver", SILVER), ("Mid Grey", MID_GREY), ("Pastel Pink", PASTEL_PINK)].into_iter().map(|(n, t)| Theme { name: n.into(), tokens: t }).collect()
}

pub const DEFAULT: &str = "Graphite Grey";

// ---- the current theme, for code that paints by hand -------------------------------------------------------------------

static CURRENT: RwLock<Tokens> = RwLock::new(GRAPHITE);

pub fn current() -> Tokens {
    CURRENT.read().map(|t| *t).unwrap_or(GRAPHITE)
}

pub fn text() -> Color32 {
    current().text
}
pub fn dim() -> Color32 {
    current().dim
}
pub fn bg() -> Color32 {
    current().bg
}
pub fn surface() -> Color32 {
    current().surface
}
pub fn surface_hi() -> Color32 {
    current().surface_hi
}
pub fn border() -> Color32 {
    current().border
}
pub fn accent() -> Color32 {
    current().accent
}
pub fn ok() -> Color32 {
    current().ok
}
pub fn warn() -> Color32 {
    current().warn
}
pub fn err() -> Color32 {
    current().error
}
pub fn info() -> Color32 {
    current().info
}

/// the 3D view's clear colour: the page colour, a little lifted
pub fn viewport_clear() -> [f64; 3] {
    let b = current().bg;
    let lift = if current().is_dark() { 6.0 } else { 0.0 };
    [(b.r() as f64 + lift) / 255.0, (b.g() as f64 + lift) / 255.0, (b.b() as f64 + lift) / 255.0]
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Make `t` the theme of the whole application.
pub fn apply(ctx: &egui::Context, t: &Tokens) {
    if let Ok(mut cur) = CURRENT.write() {
        *cur = *t;
    }
    let mut v = if t.is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = None;
    v.panel_fill = t.bg;
    v.window_fill = t.surface;
    v.extreme_bg_color = t.bg;
    v.code_bg_color = t.bg;
    v.faint_bg_color = mix(t.surface, t.surface_hi, 0.5);
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = t.accent;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.error;
    v.selection.bg_fill = mix(t.surface, t.accent, 0.35);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.surface;
    w.noninteractive.weak_bg_fill = t.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = t.surface_hi;
    w.inactive.weak_bg_fill = t.surface_hi;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = mix(t.surface_hi, t.accent, 0.22);
    w.hovered.weak_bg_fill = mix(t.surface_hi, t.accent, 0.22);
    w.hovered.bg_stroke = Stroke::new(1.0, mix(t.border, t.accent, 0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, t.text);
    w.hovered.corner_radius = r;
    w.active.bg_fill = mix(t.surface_hi, t.accent, 0.4);
    w.active.weak_bg_fill = mix(t.surface_hi, t.accent, 0.4);
    w.active.bg_stroke = Stroke::new(1.0, t.accent);
    w.active.fg_stroke = Stroke::new(1.5, t.text);
    w.active.corner_radius = r;
    w.open.bg_fill = t.surface_hi;
    w.open.weak_bg_fill = t.surface_hi;
    w.open.bg_stroke = Stroke::new(1.0, t.border);
    w.open.fg_stroke = Stroke::new(1.0, t.text);
    w.open.corner_radius = r;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.menu_margin = egui::Margin::same(8);
        s.spacing.window_margin = egui::Margin::same(12);
    });
}

// ---- files ------------------------------------------------------------------------------------------------------------

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

pub fn from_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

pub fn serialise(t: &Theme) -> String {
    let mut tk = t.tokens;
    let mut s = format!("name={}\n", t.name);
    for (k, _, col) in tk.fields() {
        s += &format!("{k}={}\n", to_hex(*col));
    }
    s
}

/// A theme from text; a colour that is missing or not `#rrggbb` is an error, so a half-written file never half-applies.
pub fn parse(text: &str) -> Result<Theme, String> {
    let mut name = None;
    let mut t = GRAPHITE;
    let mut seen = 0;
    let lines: Vec<(&str, &str)> = text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect();
    for (k, v) in &lines {
        if *k == "name" {
            name = Some(v.to_string());
        }
    }
    for (k, label, col) in t.fields() {
        let v = lines.iter().find(|(lk, _)| *lk == k).map(|(_, v)| *v).ok_or_else(|| format!("the colour \"{label}\" ({k}) is missing"))?;
        *col = from_hex(v).ok_or_else(|| format!("\"{v}\" is not a colour for {label}: write it like #1a2b3c"))?;
        seen += 1;
    }
    debug_assert_eq!(seen, 11);
    let name = name.filter(|n| !n.is_empty()).ok_or("the theme has no name= line")?;
    Ok(Theme { name, tokens: t })
}

pub fn dir() -> PathBuf {
    rage_toolkit_core::game::cache_dir().parent().map(|p| p.join("themes")).unwrap_or_else(|| PathBuf::from("rage_toolkit_themes"))
}

fn file_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{}.theme", safe.trim())
}

pub fn load_custom(dir: &Path) -> Vec<Theme> {
    let mut v: Vec<Theme> = fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.path().extension().map(|x| x == "theme").unwrap_or(false)).filter_map(|e| fs::read_to_string(e.path()).ok()).filter_map(|t| parse(&t).ok()).collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

pub fn save_custom(dir: &Path, t: &Theme) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join(file_name(&t.name)), serialise(t))
}

pub fn delete_custom(dir: &Path, name: &str) {
    let _ = fs::remove_file(dir.join(file_name(name)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_round_trip_through_text() {
        assert_eq!(from_hex("#0a1B2c"), Some(Color32::from_rgb(10, 27, 44)));
        assert_eq!(to_hex(Color32::from_rgb(10, 27, 44)), "#0a1b2c");
        assert!(from_hex("#12345").is_none() && from_hex("zzzzzz").is_none() && from_hex("#é12345").is_none());
    }

    #[test]
    fn every_built_in_theme_survives_saving_and_loading() {
        for t in builtin() {
            let back = parse(&serialise(&t)).unwrap();
            assert_eq!(back, t);
        }
        assert_eq!(builtin().len(), 9);
    }

    #[test]
    fn a_broken_theme_file_is_refused_with_a_reason() {
        let mut ok = serialise(&builtin()[0]);
        assert!(parse(&ok).is_ok());
        ok = ok.replace("accent=#", "accent=nothex#");
        assert!(parse(&ok).unwrap_err().contains("Accent"));
        assert!(parse("name=x\nbg=#000000\n").unwrap_err().contains("missing"));
        assert!(parse(&serialise(&builtin()[0]).replace("name=AMOLED Black", "name=")).is_err());
    }

    #[test]
    fn dark_and_light_are_told_apart_and_custom_themes_are_kept_on_disk() {
        assert!(AMOLED.is_dark() && GRAPHITE.is_dark() && DARK_BLUE.is_dark() && MID_GREY.is_dark());
        assert!(!LIGHT.is_dark() && !LIGHT_GREEN.is_dark() && !SOFT_WHITE.is_dark() && !SILVER.is_dark() && !PASTEL_PINK.is_dark());
        let d = std::env::temp_dir().join(format!("rage_toolkit_themes_{}", std::process::id()));
        let t = Theme { name: "My: theme".into(), tokens: DARK_BLUE };
        save_custom(&d, &t).unwrap();
        let got = load_custom(&d);
        assert_eq!(got, vec![t.clone()]);
        delete_custom(&d, &t.name);
        assert!(load_custom(&d).is_empty());
        let _ = fs::remove_dir_all(d);
    }
}

/// the frame of the top bar, status bar and side panels: a surface with a thin border, away from the page
pub fn panel_frame() -> egui::Frame {
    let t = current();
    egui::Frame::new().fill(t.surface).inner_margin(egui::Margin::symmetric(12, 8))
}
