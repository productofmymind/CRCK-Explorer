//! The first screen: what do you want to do? One card for each thing CRCK Explorer does.
use crate::tools::Page;
use eframe::egui::{self, Align2, Color32, Rect, Sense};
use std::path::Path;

/// where a card leads
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Go {
    Explorer,
    OpenModel,
    VehicleFromGame,
    /// the archetype editor: open a file or start a new one
    Ytyp,
    Tool(Page),
}

struct Card {
    title: &'static str,
    blurb: &'static str,
    accent: Color32,
    glyph: Glyph,
    go: Go,
    /// needs GTA V to be found on this PC
    needs_game: bool,
}

#[derive(Clone, Copy)]
enum Glyph {
    Folder,
    Model,
    Archive,
    /// a box: a prop / archetype
    Cube,
    Car,
    Picture,
    Shirt,
    Rename,
    Shield,
}

const YELLOW: Color32 = Color32::from_rgb(244, 200, 88);
const BLUE: Color32 = Color32::from_rgb(110, 190, 240);
const ORANGE: Color32 = Color32::from_rgb(255, 170, 80);
const GREEN: Color32 = Color32::from_rgb(140, 210, 140);
const VIOLET: Color32 = Color32::from_rgb(196, 166, 255);

fn sections() -> Vec<(&'static str, Vec<Card>)> {
    let c = |title, blurb, accent, glyph, go, needs_game| Card { title, blurb, accent, glyph, go, needs_game };
    vec![
        (
            "Look at things",
            vec![
                c("Browse the game's files", "Folders and archives of GTA V and your own folders, like classic modding tools. Search by file or folder name.", YELLOW, Glyph::Folder, Go::Explorer, false),
                c("Look at a model", "Open a .ydr, .ydd or .yft with its textures: parts, detail levels, paint, normal maps.", BLUE, Glyph::Model, Go::OpenModel, false),
                c("A vehicle from the game", "Type a spawn name like adder or police3 and see it with the textures the game gives it.", BLUE, Glyph::Car, Go::VehicleFromGame, true),
            ],
        ),
        (
            "Make and edit",
            vec![
                c("Images to YTD", "Drop images or folders: all into one YTD, or each image into its own.", ORANGE, Glyph::Picture, Go::Tool(Page::Textures), false),
                c("Archetype editor", "Open a .ytyp, or start a new one, and edit its props, time archetypes and MLO interiors; checks before it saves.", VIOLET, Glyph::Cube, Go::Ytyp, false),
                c("Clothing pack", "Drop .ydd models and .ytd textures; they are paired, numbered, checked and built into a FiveM clothing resource.", GREEN, Glyph::Shirt, Go::Tool(Page::Clothing), false),
                c("Batch rename", "Rename many files at once: numbers, texture letters, find and replace. Preview and undo.", VIOLET, Glyph::Rename, Go::Tool(Page::Rename), false),
            ],
        ),
        (
            "Check and unpack",
            vec![
                c("Check a pack for mistakes", "Names that must match, missing files, FiveM size limits: before the game finds out.", GREEN, Glyph::Shield, Go::Tool(Page::Validate), false),
                c("Archives (RPF)", "List, extract and build open RPF archives.", YELLOW, Glyph::Archive, Go::Tool(Page::Archives), false),
            ],
        ),
    ]
}

const CARD_W: f32 = 318.0;
const CARD_H: f32 = 98.0;

fn card(ui: &mut egui::Ui, c: &Card, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), if enabled { Sense::click() } else { Sense::hover() });
    let hot = enabled && resp.hovered();
    let p = ui.painter();
    p.rect_filled(rect, 9.0, if hot { crate::theme::surface_hi() } else { crate::theme::surface() });
    if hot {
        p.rect_filled(Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height())), 2.0, c.accent);
    }
    // the icon tile
    let tile = Rect::from_min_size(rect.min + egui::vec2(16.0, (CARD_H - 46.0) / 2.0), egui::vec2(46.0, 46.0));
    p.rect_filled(tile, 10.0, c.accent.gamma_multiply(if enabled { 0.22 } else { 0.1 }));
    match c.glyph {
        Glyph::Cube => {
            let col = if enabled { c.accent } else { crate::theme::dim() };
            let (m, r) = (tile.center(), 12.0);
            let pt = |a: f32, k: f32| m + egui::vec2(a.to_radians().cos(), a.to_radians().sin()) * (r * k);
            let hex: Vec<egui::Pos2> = (0..6).map(|i| pt(-90.0 + 60.0 * i as f32, 1.0)).collect();
            let st = egui::Stroke::new(1.8, col);
            p.add(egui::Shape::closed_line(hex.clone(), st));
            for i in [1, 3, 5] {
                p.line_segment([m, hex[i]], st);
            }
        }
        _ => {
            let col = if enabled { c.accent } else { crate::theme::dim() };
            let st = egui::Stroke::new(1.8, col);
            let m = tile.center();
            let pts = |v: &[(f32, f32)]| -> Vec<egui::Pos2> { v.iter().map(|(x, y)| m + egui::vec2(*x, *y)).collect() };
            match c.glyph {
                Glyph::Picture => {
                    p.rect_stroke(Rect::from_center_size(m, egui::vec2(26.0, 20.0)), 3.0, st, egui::StrokeKind::Inside);
                    p.circle_stroke(m + egui::vec2(-5.0, -3.5), 2.5, st);
                    p.add(egui::Shape::line(pts(&[(-12.0, 8.0), (-4.0, 1.0), (1.0, 6.0), (6.0, 1.0), (12.0, 7.0)]), st));
                }
                Glyph::Folder => {
                    p.add(egui::Shape::closed_line(pts(&[(-13.0, -8.0), (-5.0, -8.0), (-2.0, -5.0), (13.0, -5.0), (13.0, 9.0), (-13.0, 9.0)]), st));
                }
                Glyph::Model => {
                    p.circle_stroke(m, 11.0, st);
                    let ring: Vec<(f32, f32)> = (0..=24).map(|i| { let a = i as f32 / 24.0 * std::f32::consts::TAU; (a.cos() * 11.0, a.sin() * 4.0) }).collect();
                    p.add(egui::Shape::line(pts(&ring), st));
                    p.line_segment([m + egui::vec2(0.0, -11.0), m + egui::vec2(0.0, 11.0)], st);
                }
                Glyph::Car => {
                    p.add(egui::Shape::closed_line(pts(&[(-13.0, 4.0), (-13.0, -1.0), (-8.0, -2.0), (-4.0, -8.0), (5.0, -8.0), (9.0, -2.0), (13.0, -1.0), (13.0, 4.0)]), st));
                    p.circle_filled(m + egui::vec2(-7.0, 4.0), 3.6, crate::theme::surface());
                    p.circle_stroke(m + egui::vec2(-7.0, 4.0), 3.2, st);
                    p.circle_filled(m + egui::vec2(7.0, 4.0), 3.6, crate::theme::surface());
                    p.circle_stroke(m + egui::vec2(7.0, 4.0), 3.2, st);
                }
                Glyph::Archive => {
                    p.rect_stroke(Rect::from_min_max(m + egui::vec2(-13.0, -10.0), m + egui::vec2(13.0, -4.0)), 2.0, st, egui::StrokeKind::Inside);
                    p.rect_stroke(Rect::from_min_max(m + egui::vec2(-11.0, -4.0), m + egui::vec2(11.0, 11.0)), 2.0, st, egui::StrokeKind::Inside);
                    p.line_segment([m + egui::vec2(-3.5, 2.0), m + egui::vec2(3.5, 2.0)], st);
                }
                Glyph::Shirt => {
                    p.add(egui::Shape::closed_line(pts(&[(-4.0, -12.0), (-13.0, -8.0), (-10.0, -1.0), (-7.0, -3.0), (-7.0, 12.0), (7.0, 12.0), (7.0, -3.0), (10.0, -1.0), (13.0, -8.0), (4.0, -12.0), (0.0, -8.0)]), st));
                }
                Glyph::Rename => {
                    for (y, x1) in [(-7.0, 3.0), (0.0, 7.0), (7.0, 1.0)] {
                        p.line_segment([m + egui::vec2(-13.0, y), m + egui::vec2(x1, y)], st);
                    }
                    p.line_segment([m + egui::vec2(11.0, -11.0), m + egui::vec2(11.0, 11.0)], st);
                    p.line_segment([m + egui::vec2(8.0, -11.0), m + egui::vec2(14.0, -11.0)], st);
                    p.line_segment([m + egui::vec2(8.0, 11.0), m + egui::vec2(14.0, 11.0)], st);
                }
                _ => {
                    p.add(egui::Shape::closed_line(pts(&[(0.0, -13.0), (11.0, -9.0), (11.0, 0.0), (0.0, 13.0), (-11.0, 0.0), (-11.0, -9.0)]), st));
                    p.add(egui::Shape::line(pts(&[(-5.0, 0.0), (-1.0, 4.5), (6.0, -4.0)]), st));
                }
            }
        }
    }
    let (title_col, text_col) = if enabled { (crate::theme::text(), crate::theme::dim()) } else { (crate::theme::dim(), crate::theme::surface_hi()) };
    // the title and the blurb together are centred in the card, like the icon
    let galley = p.layout(c.blurb.to_string(), egui::FontId::proportional(12.5), text_col, CARD_W - 92.0);
    let top = rect.top() + (CARD_H - (20.0 + 4.0 + galley.size().y)) / 2.0;
    p.text(egui::pos2(rect.left() + 76.0, top + 10.0), Align2::LEFT_CENTER, c.title, egui::FontId::proportional(16.5), title_col);
    p.galley(egui::pos2(rect.left() + 76.0, top + 24.0), galley, text_col);
    if !enabled && c.needs_game {
        resp.on_hover_text("GTA V was not found on this PC")
    } else {
        resp
    }
}

/// The footer: whether GTA V was found, and the switch for showing this screen at start.
pub fn footer(ui: &mut egui::Ui, game_dir: Option<&Path>, show_at_start: &mut bool) {
    ui.add_space(4.0);
    match game_dir {
        Some(d) => {
            ui.label(egui::RichText::new(format!("GTA V found: {}  (only ever read, never changed)", d.display())).color(crate::theme::ok()));
        }
        None => {
            ui.label(egui::RichText::new("GTA V was not found on this PC. The Explorer, the Viewer and the Tools still work on your own files and folders.").color(crate::theme::warn()));
        }
    }
    ui.checkbox(show_at_start, "Show this screen when CRCK Explorer starts").on_hover_text("Off: CRCK Explorer starts in the Explorer. The Home button at the top left brings this screen back.");
    ui.add_space(2.0);
}

/// The cards. Returns where the user wants to go.
pub fn show(ui: &mut egui::Ui, game_found: bool, logo: Option<&egui::TextureHandle>) -> Option<Go> {
    let mut go = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // Exactly three cards and their gaps, so the block has no spare room on the right.
        let width = (3.0 * CARD_W + 2.0 * 14.0).min(ui.available_width());
        let pad = ((ui.available_width() - width) / 2.0).max(10.0);
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_max_width(width);
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    if let Some(t) = logo {
                        ui.add(egui::Image::new((t.id(), egui::vec2(84.0, 84.0))));
                    }
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("CRCK Explorer").size(36.0).strong());
                        ui.label(egui::RichText::new("GTA V and FiveM modding tools in one place. What do you want to do?").size(16.0).color(crate::theme::dim()));
                    });
                });
                for (name, cards) in sections() {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new(name).size(14.0).strong().color(crate::theme::info()));
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(14.0, 14.0);
                        for c in &cards {
                            let enabled = !c.needs_game || game_found;
                            if card(ui, c, enabled).clicked() {
                                go = Some(c.go);
                            }
                        }
                    });
                }
                ui.add_space(16.0);
            });
        });
    });
    go
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cards_lead_somewhere_once_and_the_game_ones_say_so() {
        let cards: Vec<Card> = sections().into_iter().flat_map(|s| s.1).collect();
        // Pack, Handling, Livery and Siren are taken off Home for now (still on the Tools page)
        for p in Page::ALL {
            let n = cards.iter().filter(|c| c.go == Go::Tool(p)).count();
            let hidden = matches!(p, Page::Pack | Page::Handling | Page::Livery | Page::Siren);
            assert_eq!(n, if hidden { 0 } else { 1 }, "{p:?}");
        }
        assert!(cards.iter().any(|c| c.go == Go::Explorer) && cards.iter().any(|c| c.go == Go::OpenModel));
        assert!(cards.iter().any(|c| c.go == Go::VehicleFromGame && c.needs_game));
        assert!(cards.iter().all(|c| !c.title.is_empty() && c.blurb.len() > 20));
    }
}
