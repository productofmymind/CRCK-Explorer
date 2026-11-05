//! A scripted self-test of the editor in the real app: `crck-explorer --world-view x,y,z --world-selftest`. It makes a map of three props in
//! front of the camera and drives the interface the way a person does, by feeding mouse and key events to the window: it clicks, Ctrl-clicks
//! and box-selects, drags the arrows and the plane handle of the gizmo, copies with Shift, snaps to the grid and to the ground, deletes,
//! copies, pastes, undoes, and chooses a LOD parent by clicking; after each step it checks what happened to the entities. The results go to
//! `crck_world_selftest.txt` in the temp folder (PASS or FAIL for each check) and to stderr, and the window closes when it is done. It works in a
//! scratch project, and the game's own entities are hidden so nothing but the three props can be hit.
use super::*;
use std::collections::VecDeque;

/// events to feed the next frame, and the modifier keys held in it
pub(super) struct Synthetic {
    pub events: Vec<egui::Event>,
    pub modifiers: egui::Modifiers,
}

type Events = Box<dyn Fn(&World) -> Option<Vec<egui::Event>>>;

enum Step {
    /// run something now (set up, remember a value)
    Do(Box<dyn Fn(&mut World)>),
    /// events for the next frame, worked out from where things are on the screen now
    Events(Events, egui::Modifiers),
    Wait(u32),
    /// wait until this holds (at most so many frames)
    Until(&'static str, Box<dyn Fn(&World) -> bool>, u32),
    /// what has to be true now
    Check(&'static str, Box<dyn Fn(&mut World) -> Result<(), String>>),
}

/// values the steps hand to each other
#[derive(Default)]
pub(super) struct Vars {
    ids: Vec<u32>,
    saved: Option<EntityEdit>,
    count: usize,
    drag: Option<(Pos2, egui::Vec2)>,
    /// the game map opened for editing, and how many entities the game has in it
    map: Option<(u32, usize)>,
}

pub(super) struct Script {
    steps: VecDeque<Step>,
    wait: u32,
    waiting: u32,
    results: Vec<String>,
    failed: usize,
}

const CTRL: egui::Modifiers = egui::Modifiers { alt: false, ctrl: true, shift: false, mac_cmd: false, command: true };
const SHIFT: egui::Modifiers = egui::Modifiers { alt: false, ctrl: false, shift: true, mac_cmd: false, command: false };
const NONE: egui::Modifiers = egui::Modifiers { alt: false, ctrl: false, shift: false, mac_cmd: false, command: false };

fn mv(p: Pos2) -> egui::Event {
    egui::Event::PointerMoved(p)
}

fn button(p: Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers }
}

fn key(k: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![egui::Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers }, egui::Event::Key { key: k, physical_key: Some(k), pressed: false, repeat: false, modifiers }]
}

fn ev(modifiers: egui::Modifiers, f: impl Fn(&World) -> Option<Vec<egui::Event>> + 'static) -> Step {
    Step::Events(Box::new(f), modifiers)
}

/// a key pressed and released with these modifiers held
fn chord(k: egui::Key, modifiers: egui::Modifiers) -> Step {
    ev(modifiers, move |_| Some(key(k, modifiers)))
}

fn doit(f: impl Fn(&mut World) + 'static) -> Step {
    Step::Do(Box::new(f))
}

fn check(name: &'static str, f: impl Fn(&mut World) -> Result<(), String> + 'static) -> Step {
    Step::Check(name, Box::new(f))
}

/// where entity `k` of the test is on the screen
fn entity_pos(w: &World, k: usize) -> Option<Pos2> {
    let id = *w.vars.ids.get(k)?;
    let pick = w.scene.as_ref()?.picks.iter().find(|p| p.id == id)?;
    let (vp, rect) = w.last_view?;
    project(&vp, pick.c3, rect)
}

fn press(p: Pos2, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![mv(p), button(p, true, modifiers)]
}

fn release(p: Pos2, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![mv(p), button(p, false, modifiers)]
}

/// a click on entity `k`: press, then release in the next frame
fn click_entity(k: usize, modifiers: egui::Modifiers) -> Vec<Step> {
    vec![ev(modifiers, move |w| Some(press(entity_pos(w, k)?, modifiers))), ev(modifiers, move |w| Some(release(entity_pos(w, k)?, modifiers))), Step::Wait(2)]
}

/// a drag: `from` gives where it starts and how far it goes (evaluated once, when it begins), then press, four moves and release
fn drag(from: impl Fn(&World) -> Option<(Pos2, egui::Vec2)> + 'static, modifiers: egui::Modifiers) -> Vec<Step> {
    let mut v = vec![doit(move |w| w.vars.drag = from(w))];
    v.push(ev(modifiers, move |w| w.vars.drag.map(|(p, _)| press(p, modifiers))));
    for k in 1..=4 {
        v.push(ev(modifiers, move |w| w.vars.drag.map(|(p, d)| vec![mv(p + d * (k as f32 / 4.0))])));
    }
    v.push(ev(modifiers, move |w| w.vars.drag.map(|(p, d)| release(p + d, modifiers))));
    v.push(Step::Wait(3));
    v
}

/// the start of the arrow of axis `axis` (a little short of its tip) and `px` pixels along it on the screen
fn arrow_drag(axis: usize, px: f32) -> impl Fn(&World) -> Option<(Pos2, egui::Vec2)> {
    move |w| {
        let (vp, rect) = w.last_view?;
        let (pivot, frame, len) = w.gizmo_basis(w.cam.pos)?;
        let axes = [frame * Vec3::X, frame * Vec3::Y, frame * Vec3::Z];
        let o = project(&vp, pivot, rect)?;
        let tip = project(&vp, pivot + axes[axis] * len, rect)?;
        let along = (tip - o).normalized();
        Some((o + (tip - o) * 0.85, along * px))
    }
}

/// the middle of the plane handle whose normal is axis `n`, and a drag of (`dx`, `dy`) pixels
fn plane_drag(n: usize, dx: f32, dy: f32) -> impl Fn(&World) -> Option<(Pos2, egui::Vec2)> {
    move |w| {
        let (vp, rect) = w.last_view?;
        let (pivot, frame, len) = w.gizmo_basis(w.cam.pos)?;
        let axes = [frame * Vec3::X, frame * Vec3::Y, frame * Vec3::Z];
        let (u, v) = gizmo::plane_basis(n, axes);
        Some((project(&vp, pivot + (u * 0.4 + v * 0.4) * len, rect)?, egui::vec2(dx, dy)))
    }
}

fn pos_of(w: &World, k: usize) -> Option<EntityEdit> {
    w.placement_of(*w.vars.ids.get(k)?)
}

fn near(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

fn same_set(a: &[u32], b: &[u32]) -> bool {
    let (a, b): (HashSet<u32>, HashSet<u32>) = (a.iter().copied().collect(), b.iter().copied().collect());
    a == b
}

fn select_k(k: usize) -> Step {
    doit(move |w| {
        if let Some(&id) = w.vars.ids.get(k) {
            w.set_selection(vec![id]);
        }
    })
}

fn map_len(w: &World) -> usize {
    w.proj.active_map().map_or(0, |m| m.doc.len())
}

/// the steps of the self-test
fn steps() -> VecDeque<Step> {
    let mut s: Vec<Step> = vec![];
    s.push(doit(|w| {
        // nothing of the game can be hit: only the three props
        if let Some(d) = w.data() {
            w.hidden_maps = (0..d.maps.len() as u32).collect();
            w.bump();
        }
        w.show_extents = false;
        let Some(slot) = w.new_map_in_project(Some("selftest")) else { return };
        let at = w.cam.pos + w.cam.forward() * 40.0;
        let mut ids = vec![];
        for (k, name) in ["prop_bench_01a", "prop_bin_01a", "prop_traffic_01a"].iter().enumerate() {
            let p = at + Vec3::new((k as f32 - 1.0) * 9.0, 0.0, 0.0);
            if let Some(id) = w.add_entity(name, EntityEdit { pos: p.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 }) {
                ids.push(id);
            }
        }
        w.doc_op(slot, |d| {
            for i in 0..3 {
                d.set_entity_field(i, "lodDist", "500")?;
            }
            Ok(())
        });
        w.vars.ids = ids;
        w.clear_history();
        w.set_selection(vec![]);
        w.mode = ViewMode::Select;
        w.tool = Tool::Move;
        w.space = Space::World;
        w.snap_mode = SnapMode::None;
        w.layers.hd = true;
        w.layers.lod = true;
        w.cam.look_at(at, 32.0);
        w.scene = None;
    }));
    s.push(Step::Until("the three props are drawn", Box::new(|w| w.vars.ids.len() == 3 && (0..3).all(|k| entity_pos(w, k).is_some())), 1500));
    s.push(Step::Wait(10));

    // ---- selecting
    s.extend(click_entity(1, NONE));
    s.push(check("a click on a prop selects just it", |w| if w.sel == vec![w.vars.ids[1]] { Ok(()) } else { Err(format!("selected {:?}", w.sel)) }));
    s.push(ev(NONE, |w| {
        let (_, rect) = w.last_view?;
        Some(press(rect.min + egui::vec2(rect.width() * 0.88, rect.height() * 0.8), NONE))
    }));
    s.push(ev(NONE, |w| {
        let (_, rect) = w.last_view?;
        Some(release(rect.min + egui::vec2(rect.width() * 0.88, rect.height() * 0.8), NONE))
    }));
    s.push(Step::Wait(2));
    s.push(check("a click on nothing clears the selection", |w| if w.sel.is_empty() { Ok(()) } else { Err(format!("still selected {:?}", w.sel)) }));
    s.extend(click_entity(0, NONE));
    s.extend(click_entity(2, CTRL));
    s.push(check("Ctrl-click adds a prop to the selection", |w| if same_set(&w.sel, &[w.vars.ids[0], w.vars.ids[2]]) { Ok(()) } else { Err(format!("selected {:?}", w.sel)) }));
    s.extend(click_entity(2, CTRL));
    s.push(check("Ctrl-click on a selected prop takes it out", |w| if w.sel == vec![w.vars.ids[0]] { Ok(()) } else { Err(format!("selected {:?}", w.sel)) }));
    s.extend(drag(
        |w| {
            let (_, rect) = w.last_view?;
            Some((rect.min + egui::vec2(rect.width() * 0.08, rect.height() * 0.3), egui::vec2(rect.width() * 0.84, rect.height() * 0.55)))
        },
        NONE,
    ));
    s.push(check("a box round the props selects all three", |w| if same_set(&w.sel, &w.vars.ids) { Ok(()) } else { Err(format!("selected {:?} of {:?}", w.sel, w.vars.ids)) }));

    // ---- moving with an arrow
    s.push(select_k(0));
    s.push(Step::Wait(3));
    s.push(doit(|w| w.vars.saved = pos_of(w, 0)));
    s.extend(drag(arrow_drag(0, 70.0), NONE));
    s.push(check("dragging the X arrow moves the prop along X only", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if b.pos[0] - a.pos[0] > 0.3 && near(a.pos[1], b.pos[1], 0.02) && near(a.pos[2], b.pos[2], 0.02) {
            Ok(())
        } else {
            Err(format!("{:?} -> {:?}", a.pos, b.pos))
        }
    }));
    s.push(chord(egui::Key::Z, CTRL));
    s.push(Step::Wait(2));
    s.push(check("one Ctrl+Z undoes the whole drag", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if (0..3).all(|k| near(a.pos[k], b.pos[k], 1e-4)) {
            Ok(())
        } else {
            Err(format!("{:?} -> {:?}", a.pos, b.pos))
        }
    }));
    s.push(select_k(0));
    s.push(Step::Wait(3));
    s.extend(drag(plane_drag(2, 45.0, 30.0), NONE));
    s.push(check("dragging the plane handle moves it in the plane and keeps the height", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if near(a.pos[2], b.pos[2], 0.02) && ((a.pos[0] - b.pos[0]).abs() > 0.2 || (a.pos[1] - b.pos[1]).abs() > 0.2) {
            Ok(())
        } else {
            Err(format!("{:?} -> {:?}", a.pos, b.pos))
        }
    }));
    s.push(chord(egui::Key::Z, CTRL));
    s.push(Step::Wait(2));

    // ---- copying while dragging, snapping
    s.push(select_k(0));
    s.push(Step::Wait(3));
    s.push(doit(|w| w.vars.count = map_len(w)));
    s.extend(drag(arrow_drag(0, 60.0), SHIFT));
    s.push(check("Shift-dragging an arrow drags a copy and leaves the original", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if map_len(w) == w.vars.count + 1 && (0..3).all(|k| near(a.pos[k], b.pos[k], 1e-4)) && w.sel.len() == 1 && w.sel[0] != w.vars.ids[0] {
            Ok(())
        } else {
            Err(format!("{} entities (was {}), original {:?} -> {:?}, selected {:?}", map_len(w), w.vars.count, a.pos, b.pos, w.sel))
        }
    }));
    s.push(check("the copy has moved", |w| {
        let a = w.vars.saved.ok_or("no start")?;
        let c = w.placement_of(*w.sel.first().ok_or("nothing selected")?).ok_or("gone")?;
        if c.pos[0] - a.pos[0] > 0.3 { Ok(()) } else { Err(format!("{:?} -> {:?}", a.pos, c.pos)) }
    }));
    for _ in 0..2 {
        s.push(chord(egui::Key::Z, CTRL));
        s.push(Step::Wait(2));
    }
    s.push(check("undo takes the copy away again", |w| if map_len(w) == w.vars.count { Ok(()) } else { Err(format!("{} entities, expected {}", map_len(w), w.vars.count)) }));
    s.push(select_k(0));
    s.push(doit(|w| {
        w.snap_mode = SnapMode::Grid;
        w.snap = 2.0;
    }));
    s.push(Step::Wait(3));
    s.extend(drag(arrow_drag(0, 90.0), NONE));
    s.push(check("with the grid on, the position lands on a multiple of the step", |w| {
        let b = pos_of(w, 0).ok_or("gone")?;
        let (a, k) = (w.vars.saved.ok_or("no start")?, b.pos[0] / 2.0);
        if near(k, k.round(), 1e-3) && (b.pos[0] - a.pos[0]).abs() > 0.5 {
            Ok(())
        } else {
            Err(format!("x = {} (from {})", b.pos[0], a.pos[0]))
        }
    }));
    s.push(doit(|w| w.snap_mode = SnapMode::None));
    s.push(chord(egui::Key::Z, CTRL));
    s.push(Step::Wait(2));

    // ---- underground, ground
    s.push(select_k(0));
    s.push(doit(|w| {
        let id = w.vars.ids[0];
        w.nudge(&[id], Vec3::new(0.0, 0.0, -120.0));
    }));
    s.push(check("an entity can be put 120 m underground (nothing clamps the height)", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if near(b.pos[2], a.pos[2] - 120.0, 1e-3) { Ok(()) } else { Err(format!("z {} -> {}", a.pos[2], b.pos[2])) }
    }));
    s.push(check("the LOD inspector draws links to a game entity's parent and children and its distance circles", |w| {
        let data = w.data().ok_or("no data")?;
        // an entity that has both a parent and children
        let i = data.ents.iter().enumerate().find(|(i, e)| e.parent != worldload::NO_PARENT && !data.children_of(*i).is_empty()).map(|(i, _)| i as u32).ok_or("none in the game")?;
        let (sel, links) = (std::mem::take(&mut w.sel), w.show_lod_links);
        w.sel = vec![i];
        w.show_lod_links = true;
        let seg = w.lod_overlay_segments();
        w.sel = sel;
        w.show_lod_links = links;
        if seg[0].len() == 2 && !seg[1].is_empty() && seg[1].len() % 2 == 0 && !seg[2].is_empty() { Ok(()) } else { Err(format!("parent {}, children {}, circle {}", seg[0].len(), seg[1].len(), seg[2].len())) }
    }));
    s.push(check("inspecting the collision under a ray finds the file, a material and the triangle", |w| {
        let p = pos_of(w, 0).ok_or("gone")?;
        w.cursor_ray = Some((Vec3::new(p.pos[0], p.pos[1], p.pos[2] + 60.0), Vec3::new(0.0, 0.0, -1.0)));
        w.inspect_collision();
        let h = w.coll_hit.as_ref().ok_or("nothing hit")?;
        if h.file_tris > 0 && h.dist > 0.0 && w.coll_hit_model.is_some() { Ok(()) } else { Err(format!("{} triangles, {} m", h.file_tris, h.dist)) }
    }));
    s.push(doit(|w| {
        // and back up and over the ground, then drop it with the ground command
        let id = w.vars.ids[0];
        w.nudge(&[id], Vec3::new(0.0, 0.0, 220.0));
        let missed = w.snap_selection_to_ground(snap::GroundMode::Below, false);
        w.vars.count = missed;
    }));
    s.push(check("Snap to ground puts it on the game's collision under it", |w| {
        let (a, b) = (w.vars.saved.ok_or("no start")?, pos_of(w, 0).ok_or("gone")?);
        if w.vars.count == 0 && b.pos[2] < a.pos[2] + 100.0 - 5.0 && b.pos[2] > -20.0 {
            Ok(())
        } else {
            Err(format!("missed {}, z {} (was {} before the drop from {})", w.vars.count, b.pos[2], a.pos[2], a.pos[2] + 100.0))
        }
    }));
    s.push(doit(|w| {
        // back where it was
        let (id, a) = (w.vars.ids[0], w.vars.saved);
        if let Some(a) = a {
            w.set_placements(&[(id, a)]);
            w.end_groups();
        }
    }));

    // ---- delete, copy, paste, undo
    s.push(select_k(2));
    s.push(doit(|w| w.vars.count = map_len(w)));
    s.push(chord(egui::Key::Delete, NONE));
    s.push(Step::Wait(2));
    s.push(check("Delete removes the selected prop", |w| if map_len(w) + 1 == w.vars.count && w.sel.is_empty() { Ok(()) } else { Err(format!("{} entities (was {}), selected {:?}", map_len(w), w.vars.count, w.sel)) }));
    s.push(chord(egui::Key::Z, CTRL));
    s.push(Step::Wait(2));
    s.push(check("Ctrl+Z brings it back", |w| if map_len(w) == w.vars.count { Ok(()) } else { Err(format!("{} entities, expected {}", map_len(w), w.vars.count)) }));
    s.push(select_k(1));
    s.push(ev(NONE, |w| {
        let (_, rect) = w.last_view?;
        Some(vec![mv(rect.center()), egui::Event::Copy])
    }));
    s.push(Step::Wait(2));
    s.push(ev(NONE, |w| {
        let (_, rect) = w.last_view?;
        Some(vec![mv(rect.center()), egui::Event::Paste(String::new())])
    }));
    s.push(Step::Wait(3));
    s.push(check("Ctrl+C and Ctrl+V make a copy and select it", |w| {
        if map_len(w) == w.vars.count + 1 && w.sel.len() == 1 && w.sel[0] != w.vars.ids[1] { Ok(()) } else { Err(format!("{} entities (was {}), selected {:?}", map_len(w), w.vars.count, w.sel)) }
    }));
    s.push(chord(egui::Key::Z, CTRL));
    s.push(Step::Wait(2));

    // ---- choosing a LOD parent by clicking
    s.push(doit(|w| {
        let slot = w.proj.active;
        w.doc_op(slot, |d| d.set_lod_level(0, 1));
        w.set_selection(vec![w.vars.ids[1]]);
        w.pick_parent = Some(vec![w.vars.ids[1]]);
    }));
    // let the picture be made again with the entity at its new level before clicking it
    s.push(Step::Wait(8));
    s.extend(click_entity(0, NONE));
    s.push(check("clicking a higher-level prop while choosing a parent makes it the LOD parent", |w| {
        let slot = w.proj.active;
        let h = w.proj.maps[slot].doc.hierarchy();
        if h.parent.get(1) == Some(&Some(0)) && w.pick_parent.is_none() { Ok(()) } else { Err(format!("parents {:?}, still choosing {}", h.parent, w.pick_parent.is_some())) }
    }));

    // ---- a real map of the game, opened for editing
    s.push(doit(|w| {
        let Some(d) = w.data() else { return };
        let eye = w.cam.pos;
        let mut best: Option<(f32, u32)> = None;
        for cell in d.near_layers(eye.x, eye.y, 300.0, true, false) {
            for &i in cell {
                let e = &d.ents[i as usize];
                let dist = (Vec3::from(e.pos) - eye).length_squared();
                if !e.is_mlo && best.map_or(true, |b| dist < b.0) {
                    best = Some((dist, e.map));
                }
            }
        }
        if let Some((_, map)) = best {
            let count = d.ents.iter().filter(|e| e.map == map).count();
            w.vars.map = Some((map, count));
            w.import_game_map(map);
        }
    }));
    s.push(check("Open the map for editing copies every entity of one of the game's maps and hides the game's own", |w| {
        let (map, count) = w.vars.map.ok_or("no game map was found near the camera")?;
        let m = w.proj.maps.iter().find(|m| m.kind == MapKind::Game).ok_or("no copy was made")?;
        if m.doc.len() == count && w.hidden_maps.contains(&map) {
            Ok(())
        } else {
            Err(format!("the copy has {} entities, the game {count}; hidden {}", m.doc.len(), w.hidden_maps.contains(&map)))
        }
    }));
    s.push(check("a retail map checks clean: no errors from Validate", |w| {
        let slot = w.proj.maps.iter().position(|m| m.kind == MapKind::Game).ok_or("no copy")?;
        w.proj.active = slot;
        w.validate_active();
        let errors: Vec<String> = w.validation.1.iter().filter(|i| i.sev == rage_toolkit_core::ymap_doc::Sev::Error).take(3).map(|i| i.message.clone()).collect();
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }));
    s.push(check("editing the copy, saving and reopening the project gives the same entities", |w| {
        let slot = w.proj.maps.iter().position(|m| m.kind == MapKind::Game).ok_or("no copy")?;
        w.proj.active = slot;
        // move the first entity of the copy 5 m, then save with the flags and extents worked out
        let ids: Vec<u32> = vec![project::id(slot, 0)];
        w.set_selection(ids);
        w.nudge(&[project::id(slot, 0)], Vec3::new(5.0, 0.0, 0.0));
        w.save_project(true);
        let (again, warnings) = ProjectMaps::load(w.proj.dir.clone(), &w.proj.refs(), None);
        if !warnings.is_empty() {
            return Err(warnings.join("; "));
        }
        for (a, b) in w.proj.maps.iter().zip(again.maps.iter()) {
            if a.doc.len() != b.doc.len() {
                return Err(format!("{}: {} entities, reopened {}", a.name, a.doc.len(), b.doc.len()));
            }
            for i in 0..a.doc.len().min(400) {
                if a.doc.entity(i) != b.doc.entity(i) {
                    return Err(format!("{}: entity {i} is different after reopening", a.name));
                }
            }
        }
        Ok(())
    }));
    s.push(check("removing the copy brings the game's own entities back", |w| {
        let slot = w.proj.maps.iter().position(|m| m.kind == MapKind::Game).ok_or("no copy")?;
        let file = w.proj.maps[slot].file.clone();
        w.remove_project_map(slot);
        // what stays hidden is only what the DLC level hides (the versions of the map that a pack replaces)
        let expected: std::collections::HashSet<u32> = match &w.state {
            LoadState::Ready(l) => l.dlc_maps.hidden(w.dlc_level).into_iter().enumerate().filter(|(_, hide)| *hide).map(|(i, _)| i as u32).collect(),
            _ => Default::default(),
        };
        if w.hidden_maps == expected && !file.exists() {
            Ok(())
        } else {
            Err(format!("hidden maps {} (the DLC level hides {}), working copy exists {}", w.hidden_maps.len(), expected.len(), file.exists()))
        }
    }));

    // ---- the user's own model (only when CRCK_WORLD_MODELS names a folder with my_custom_prop.ydr)
    if std::env::var_os("CRCK_WORLD_MODELS").is_some() {
        s.push(doit(|w| {
            if let Some(dir) = std::env::var_os("CRCK_WORLD_MODELS") {
                w.load_model_folder(std::path::Path::new(&dir));
            }
            let at = w.cam.pos + w.cam.forward() * 30.0;
            w.add_entity("my_custom_prop", EntityEdit { pos: at.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
        }));
        s.push(Step::Until("the user's own model is loaded", Box::new(|w| w.loader.as_ref().map_or(false, |l| l.get(rage_toolkit_core::rage_joaat("my_custom_prop")).is_some())), 1500));
        s.push(check("a model file of the user's is drawn for a prop of its name", |w| {
            let ok = w.loader.as_ref().and_then(|l| l.get(rage_toolkit_core::rage_joaat("my_custom_prop"))).is_some();
            if ok { Ok(()) } else { Err("not loaded".into()) }
        }));
    }

    // ---- modes
    s.push(chord(egui::Key::Escape, NONE));
    s.push(Step::Wait(2));
    s.push(check("Esc goes back to Camera mode", |w| if w.mode == ViewMode::Camera { Ok(()) } else { Err("still in Select mode".into()) }));
    s.push(chord(egui::Key::Tab, NONE));
    s.push(Step::Wait(2));
    s.push(check("Tab switches to Select mode", |w| if w.mode == ViewMode::Select { Ok(()) } else { Err("still in Camera mode".into()) }));
    s.into()
}

impl World {
    /// Ask for the self-test once the map is loaded.
    pub fn queue_selftest(&mut self) {
        self.automation = true;
        self.script = Some(Script { steps: steps(), wait: 0, waiting: 0, results: vec![], failed: 0 });
    }

    /// The events the script wants in the next frame (the window hands them to the interface).
    pub fn take_synthetic(&mut self) -> Option<(Vec<egui::Event>, egui::Modifiers)> {
        self.synth.take().map(|s| (s.events, s.modifiers))
    }

    /// One step of the script, at the end of a frame.
    pub(super) fn script_tick(&mut self, ctx: &egui::Context) {
        if self.script.is_none() {
            return;
        }
        // frames keep coming while the script has work, even when nobody touches the window
        ctx.request_repaint();
        if !matches!(self.state, LoadState::Ready(_)) || self.ready_frames < 120 || !self.project_loaded {
            return;
        }
        let Some(mut script) = self.script.take() else { return };
        if script.wait > 0 {
            script.wait -= 1;
            self.script = Some(script);
            return;
        }
        while let Some(step) = script.steps.pop_front() {
            match step {
                Step::Do(f) => f(self),
                Step::Wait(n) => {
                    script.wait = n;
                    break;
                }
                Step::Events(f, modifiers) => {
                    if let Some(events) = f(self) {
                        self.synth = Some(Synthetic { events, modifiers });
                    } else {
                        script.results.push("FAIL could not work out where to click (a prop is not on the screen)".into());
                        script.failed += 1;
                    }
                    script.wait = 2;
                    break;
                }
                Step::Until(name, f, frames) => {
                    if f(self) {
                        script.waiting = 0;
                        continue;
                    }
                    script.waiting += 1;
                    if script.waiting > frames {
                        script.results.push(format!("FAIL gave up waiting until {name}"));
                        script.failed += 1;
                        script.steps.clear();
                    } else {
                        script.steps.push_front(Step::Until(name, f, frames));
                    }
                    break;
                }
                Step::Check(name, f) => match f(self) {
                    Ok(()) => script.results.push(format!("PASS {name}")),
                    Err(why) => {
                        script.failed += 1;
                        script.results.push(format!("FAIL {name}: {why}"));
                    }
                },
            }
        }
        if script.steps.is_empty() && script.wait == 0 {
            let text = format!("{}\n{} checks, {} failed", script.results.join("\n"), script.results.len(), script.failed);
            let _ = std::fs::write(std::env::temp_dir().join("crck_world_selftest.txt"), &text);
            eprintln!("{text}");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.script = Some(script);
    }
}
