//! The panels of the World tab: the project and its maps, the entity list with the LOD hierarchy, the properties of the selection (placement,
//! ground, LOD, members), the properties of a map (name, parent, flags with what each means, extents calculated against stored, checks), and
//! the toolbar over the view.
use super::{gizmo::{Space, SnapMode}, project as projmod, snap::GroundMode, *};
use rage_toolkit_core::{
    ymap_calc::FlagLine,
    ymap_doc::{MapIssue, Sev},
    ymap_lod::{CountMode, Hierarchy, LodIssueKind},
};
use std::collections::BTreeMap;

/// what the right panel shows
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RightTab {
    Entity,
    Map,
}

/// The LOD hierarchy of a map, kept between frames: the entity list as rows (entity index, depth, has children), the hierarchy itself and
/// how many entities each LOD level has. Made again only when a map changed other than by moving entities.
pub(super) struct HierRows {
    slot: usize,
    rev: u64,
    rows: Vec<(usize, usize, bool)>,
    h: Hierarchy,
    levels: [usize; 7],
}

impl HierRows {
    /// how many rows the entity list has
    pub(super) fn rows_len(&self) -> usize {
        self.rows.len()
    }
}

fn small_heading(text: &str) -> egui::RichText {
    egui::RichText::new(text).strong()
}

fn fmt3(v: [f32; 3]) -> String {
    format!("{:.1}, {:.1}, {:.1}", v[0], v[1], v[2])
}

fn danger_text(text: &str) -> egui::RichText {
    egui::RichText::new(text).color(crate::theme::warn())
}

impl World {
    // ---- the left panel --------------------------------------------------------------------------------------------------------

    pub(super) fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        ui.heading("World");
        match &self.state {
            LoadState::NotStarted => {
                ui.weak("GTA V was not found, so there is no map to load. Set the game folder in the Explorer first.");
            }
            LoadState::Loading { index_job, world_job, tex_job, bounds_job, .. } => {
                let (a, b, c, d) = (index_job.progress(), world_job.progress(), tex_job.progress(), bounds_job.progress());
                let (done, total, what) = if d.1 > 0 { (d.0, d.1, "Indexing the collision files (first run only)") } else if c.1 > 0 { (c.0, c.1, "Indexing every texture (first run only)") } else if b.1 > 0 { (b.0, b.1, "Reading the map files") } else { (a.0, a.1, "Listing the game's files") };
                let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                ui.add(egui::ProgressBar::new(frac).text(format!("{what}  {done}/{total}")));
                ui.weak("Every map file, texture dictionary and collision file of the game is read once. The first run takes a few minutes.");
            }
            LoadState::Failed(e) => {
                ui.colored_label(crate::theme::warn(), e.as_str());
            }
            LoadState::Ready(l) => {
                ui.weak(format!("{} entities from {} map files", l.data.ents.len(), l.data.maps.len())).on_hover_text(if l.data.skipped.is_empty() { String::new() } else { format!("{} files could not be read:\n{}", l.data.skipped.len(), l.data.skipped.join("\n")) });
            }
        }
        egui::ScrollArea::vertical().id_salt("world_left_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            self.project_section(ui);
            self.entities_section(ui);
            self.view_section(ui);
            self.find_section(ui);
            self.add_section(ui);
            self.changes_section(ui);
        });
    }

    fn project_section(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(small_heading("Project")).default_open(true).show(ui, |ui| {
            let name = self.proj.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
            ui.label(egui::RichText::new(&name).strong()).on_hover_text(self.proj.dir.display().to_string());
            ui.horizontal_wrapped(|ui| {
                if ui.button("New project...").on_hover_text("Start a project in a folder you choose").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Choose or make the folder for the new project").pick_folder() {
                        self.switch_project(dir, true);
                    }
                }
                if ui.button("Open project...").on_hover_text("Open the project in a folder (it has a project.crckproj file)").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Choose the project's folder").pick_folder() {
                        if dir.join("project.crckproj").exists() {
                            self.switch_project(dir, false);
                        } else {
                            self.project_msg = "That folder has no project.crckproj file.".into();
                        }
                    }
                }
                if ui.button("Save").on_hover_text("Save the project. The flags and extents of the changed maps are worked out first. (Ctrl+S)").clicked() {
                    self.save_project(true);
                }
            });
            ui.label(format!("{} map{}", self.proj.maps.len(), if self.proj.maps.len() == 1 { "" } else { "s" }));
            let mut remove: Option<usize> = None;
            let mut write_back: Option<usize> = None;
            let mut save_one: Option<usize> = None;
            let mut activate: Option<(usize, bool)> = None;
            let rows: Vec<(String, usize, bool, MapKind)> = self.proj.maps.iter().map(|m| (m.name.clone(), m.doc.len(), m.dirty(), m.kind)).collect();
            for (slot, (name, count, dirty, kind)) in rows.iter().enumerate() {
                ui.horizontal(|ui| {
                    let on = self.proj.active == slot;
                    let label = format!("{}{}  ({count})", name, if *dirty { " •" } else { "" });
                    let tip = match kind {
                        MapKind::New => "A map made in this project",
                        MapKind::File => "Imported from a .ymap of yours: the original is only written when you choose Save to the original",
                        MapKind::Game => "A copy of one of the game's maps, open for editing",
                    };
                    if ui.selectable_label(on, label).on_hover_text(format!("{tip}. New entities go into the map marked here.")).clicked() {
                        activate = Some((slot, false));
                    }
                    ui.menu_button("...", |ui| {
                        if ui.button("Properties").clicked() {
                            activate = Some((slot, true));
                            ui.close();
                        }
                        if ui.button("Save this map").on_hover_text("Work out its flags and extents and write its working copy").clicked() {
                            save_one = Some(slot);
                            ui.close();
                        }
                        if ui.add_enabled(*kind == MapKind::File, egui::Button::new("Save to the original")).on_disabled_hover_text("Only a map imported from a file can be written back").clicked() {
                            write_back = Some(slot);
                            ui.close();
                        }
                        if ui.button(danger_text("Remove from project")).on_hover_text("Deletes the working copy. The file it came from is not touched.").clicked() {
                            remove = Some(slot);
                            ui.close();
                        }
                    });
                });
            }
            if let Some((slot, props)) = activate {
                self.proj.active = slot;
                if props {
                    self.right_tab = RightTab::Map;
                }
                self.bump_docs();
            }
            if let Some(slot) = save_one {
                self.project_msg = match self.save_map(slot) {
                    Ok(()) => format!("Saved {}.", self.proj.maps[slot].name),
                    Err(e) => format!("{e:#}"),
                };
            }
            if let Some(slot) = write_back {
                self.project_msg = match self.proj.write_source(slot) {
                    Ok(p) => format!("Wrote {} (the first version is kept as .bak).", p.display()),
                    Err(e) => format!("{e:#}"),
                };
            }
            if let Some(slot) = remove {
                let sure = rfd::MessageDialog::new().set_title("Remove the map").set_description("Remove this map from the project and delete its working copy? The file it came from is not touched.").set_buttons(rfd::MessageButtons::YesNo).show();
                if sure == rfd::MessageDialogResult::Yes {
                    self.remove_project_map(slot);
                }
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("New YMAP").on_hover_text("An empty map in the project").clicked() {
                    self.new_map_in_project(None);
                }
                if ui.button("Menyoo XML...").on_hover_text("Import a Menyoo / Spooner placement file as a new map").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("Menyoo placements", &["xml"]).pick_file() {
                        self.import_menyoo(&f);
                    }
                }
                if ui.button("Import YMAP...").on_hover_text("Bring in a .ymap file of yours").clicked() {
                    if let Some(files) = rfd::FileDialog::new().add_filter("Map file", &["ymap"]).set_title("Import map files").pick_files() {
                        for f in files {
                            self.import_ymap_file(&f);
                        }
                    }
                }
            });
            if !self.proj.maps.is_empty() && ui.button("LOD lights").on_hover_text("Make the _lodlights and _distantlights maps from the lights of the models the maps place").clicked() {
                let name = self.proj.active_map().map_or("project".to_string(), |m| m.name.clone());
                self.generate_lod_lights(&name);
            }
            if !self.proj.maps.is_empty() && ui.button("Manifest...").on_hover_text("Write a _manifest.ymf.xml: which .ytyp files each map needs").clicked() {
                self.export_manifest();
            }
            if self.proj.maps.len() > 1 {
                if ui.button("Recalculate all").on_hover_text("Work out the extents (from scratch) and the flags of every map from its entities").clicked() {
                    self.recalculate_all();
                }
            }
            if self.proj.maps.is_empty() {
                ui.weak("No maps yet. Make one, import one, or select a game entity and choose \"Open the map for editing\".");
            }
            if !self.project_msg.is_empty() {
                ui.label(egui::RichText::new(&self.project_msg).small());
            }
            if self.dirty_since.is_some() {
                ui.weak("Saving...");
            } else if !self.proj.maps.is_empty() || !self.edits.is_empty() || !self.deleted.is_empty() {
                ui.label(egui::RichText::new("Saved. It comes back next time.").small().color(crate::theme::ok()));
            }
        });
    }

    /// The extents the entities of map `slot` give, worked out again only when a map changed.
    pub(super) fn calc_extents_cached(&mut self, slot: usize) -> Option<MapExtents> {
        let rev = self.proj_rev;
        if self.extents_cache.as_ref().map_or(true, |(s, r, _)| *s != slot || *r != rev) {
            let calc = match (self.data(), self.proj.maps.get(slot)) {
                (Some(d), Some(m)) => m.doc.calc_extents(&Self::arch_bounds(&d)),
                _ => None,
            };
            self.extents_cache = Some((slot, rev, calc));
        }
        self.extents_cache.as_ref().and_then(|c| c.2)
    }

    /// What each flag of map `slot` says and what its contents say it should be, worked out again only when a map changed.
    fn flag_lines_cached(&mut self, slot: usize) -> Vec<FlagLine> {
        let rev = self.proj_rev;
        if self.flags_cache.as_ref().map_or(true, |(s, r, _)| *s != slot || *r != rev) {
            let lines = self.proj.maps.get(slot).map(|m| m.doc.flag_report()).unwrap_or_default();
            self.flags_cache = Some((slot, rev, lines));
        }
        self.flags_cache.as_ref().map(|c| c.2.clone()).unwrap_or_default()
    }

    /// the LOD hierarchy and the entity list of map `slot`, rebuilt only when the map changed
    pub(super) fn hier_rows(&mut self, slot: usize) -> &HierRows {
        let stale = self.hier.as_ref().map_or(true, |h| h.slot != slot || h.rev != self.struct_rev);
        if stale {
            let (rows, h, levels) = match self.proj.maps.get(slot) {
                Some(m) => {
                    let h = m.doc.hierarchy();
                    let mut rows = Vec::with_capacity(m.doc.len());
                    // roots in file order, each followed by everything below it
                    for &r in &h.roots {
                        rows.push((r, 0, !h.children[r].is_empty()));
                        let mut stack: Vec<(usize, usize)> = h.children[r].iter().rev().map(|&c| (c, 1)).collect();
                        while let Some((n, depth)) = stack.pop() {
                            rows.push((n, depth, !h.children[n].is_empty()));
                            stack.extend(h.children[n].iter().rev().map(|&c| (c, depth + 1)));
                        }
                    }
                    let mut levels = [0usize; 7];
                    for i in 0..m.doc.len() {
                        if let Some(e) = m.doc.entity(i) {
                            levels[(e.lod_level as usize).min(6)] += 1;
                        }
                    }
                    (rows, h, levels)
                }
                None => (vec![], Hierarchy::default(), [0; 7]),
            };
            self.hier = Some(HierRows { slot, rev: self.struct_rev, rows, h, levels });
        }
        self.hier.as_ref().unwrap()
    }

    fn entities_section(&mut self, ui: &mut egui::Ui) {
        let slot = self.proj.active;
        let Some(map_name) = self.proj.maps.get(slot).map(|m| m.name.clone()) else { return };
        egui::CollapsingHeader::new(small_heading(&format!("Entities of {map_name}"))).default_open(true).show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.entity_filter).hint_text("Filter by model name").desired_width(f32::INFINITY));
            let filter = self.entity_filter.trim().to_lowercase();
            let rows: Vec<(usize, usize, bool)> = {
                let all = self.hier_rows(slot).rows.clone();
                if filter.is_empty() {
                    all
                } else {
                    all.into_iter().filter(|(i, _, _)| self.proj.maps[slot].doc.entity(*i).map_or(false, |e| self.name(e.archetype_hash).to_lowercase().contains(&filter))).map(|(i, _, c)| (i, 0, c)).collect()
                }
            };
            ui.weak(format!("{} shown. A LOD child is indented under its parent.", rows.len()));
            let mut clicked: Option<(u32, bool, bool)> = None;
            egui::ScrollArea::vertical().id_salt("world_entities").max_height(260.0).auto_shrink([false, true]).show_rows(ui, 22.0, rows.len(), |ui, range| {
                for &(idx, depth, has_kids) in &rows[range] {
                    let id = projmod::id(slot, idx);
                    let name = self.proj.maps[slot].doc.entity(idx).map(|e| self.name(e.archetype_hash)).unwrap_or_default();
                    // plain characters only: the fonts have no tree or diamond glyphs
                    let mark = if depth > 0 { "›" } else { "•" };
                    let kids = if has_kids { "  (parent)" } else { "" };
                    let text = format!("{}{mark} {idx}  {name}{kids}", "    ".repeat(depth.min(6)));
                    let r = ui.selectable_label(self.is_selected(id), text);
                    if r.clicked() {
                        clicked = Some((id, ui.input(|i| i.modifiers.command || i.modifiers.shift), false));
                    }
                    if r.double_clicked() {
                        clicked = Some((id, false, true));
                    }
                }
            });
            if let Some((id, toggle, fly)) = clicked {
                if let Some(children) = self.pick_parent.take() {
                    self.assign_parent(&children, Some(id));
                } else if toggle {
                    self.toggle_select(id);
                } else {
                    self.select(id, fly);
                }
            }
            ui.horizontal(|ui| {
                let single = self.sel.len() == 1 && projmod::split(self.sel[0]).map_or(false, |(s, _)| s == slot);
                if ui.add_enabled(single, egui::Button::new("Up")).on_hover_text("Move the selected entity one place earlier in the file").on_disabled_hover_text("Select one entity of this map").clicked() {
                    self.reorder_selected(-1);
                }
                if ui.add_enabled(single, egui::Button::new("Down")).on_hover_text("Move the selected entity one place later in the file").on_disabled_hover_text("Select one entity of this map").clicked() {
                    self.reorder_selected(1);
                }
            });
        });
    }

    fn view_section(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(small_heading("What to show")).default_open(std::env::var_os("CRCK_WORLD_OPEN").is_some()).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Max LOD");
                let names = [(5u8, "Everything (orphan HD)"), (0, "HD"), (1, "LOD"), (2, "SLOD 1"), (3, "SLOD 2"), (4, "SLOD 3"), (6, "SLOD 4")];
                egui::ComboBox::from_id_salt("max_lod").selected_text(names.iter().find(|n| n.0 == self.max_lod).map_or("?", |n| n.1)).show_ui(ui, |ui| {
                    for (v, n) in names {
                        ui.selectable_value(&mut self.max_lod, v, n);
                    }
                })
                .response
                .on_hover_text("The most detailed level that is drawn. Everything shows each level out to its own distance and gives way to its detailed children when you come close; choose SLOD or LOD to see the whole map as that level.");
                ui.checkbox(&mut self.layers.interiors, "Interiors").on_hover_text("Interior (MLO) placements");
                ui.add_enabled(self.max_lod != 5, egui::Checkbox::new(&mut self.only_level, "Only this level")).on_hover_text("Draw just the chosen level, not the coarser ones above it, to inspect one level of the hierarchy");
                ui.checkbox(&mut self.stream_filter, "Stream like the game").on_hover_text("Only look at the maps the game would have loaded here (the camera inside the map's streaming box). Off looks at every map out to the view distance, which is slower");
                ui.checkbox(&mut self.wait_kids, "Wait for children").on_hover_text("Keep a LOD model drawn until the models that replace it have loaded, so no hole shows");
                ui.checkbox(&mut self.occlusion, "Occlusion").on_hover_text("Leave out what the game's own occluders (buildings' hidden-surface models and boxes) hide from the camera");
            });
            ui.horizontal(|ui| {
                if ui.button("Check LOD links").on_hover_text("Find game entities whose LOD parent is missing, not coarser than the child, or that count the wrong number of children").clicked() {
                    self.lod_report = self.data().map(|d| d.lod_problems(40));
                }
                if self.lod_report.is_some() && ui.small_button("Clear").clicked() {
                    self.lod_report = None;
                }
            });
            if let Some((lines, total)) = &self.lod_report {
                if *total == 0 {
                    ui.label("No problems in the LOD links.");
                } else {
                    ui.label(format!("{total} problems (first {}):", lines.len()));
                    egui::ScrollArea::vertical().max_height(140.0).id_salt("lod_report").show(ui, |ui| {
                        for l in lines {
                            ui.small(l);
                        }
                    });
                }
            }
            self.lod_inspector_section(ui);
            egui::CollapsingHeader::new("Diagnostics").default_open(std::env::var_os("CRCK_WORLD_OPEN").is_some()).show(ui, |ui| {
                if let Some(sc) = &self.scene {
                    ui.small(format!("{} drawn of {} looked at · frame {:.1} ms (choose {:.1}, GPU {:.1})", sc.shown, sc.visited, sc.ms, sc.gather_ms, sc.gpu_ms));
                    let n = ["HD", "LOD", "SLOD1", "SLOD2", "SLOD3", "orphan HD", "SLOD4"];
                    let per: Vec<String> = [0usize, 1, 2, 3, 4, 6, 5].iter().filter(|&&i| sc.levels[i] > 0).map(|&i| format!("{} {}", n[i], sc.levels[i])).collect();
                    ui.small(if per.is_empty() { "no game entities drawn".to_string() } else { per.join(" · ") });
                    ui.small(format!("occlusion: {} entities hidden behind the game's occluders ({} px of the {}-wide depth picture covered)", sc.hidden, sc.occluded_px, 128));
                    ui.small(format!("{} LOD stand-ins kept while their children load · {} entities whose model does not exist", sc.waiting, sc.absent));
                }
                if let Some(l) = &self.loader {
                    ui.small(format!("models: {} loaded, {} loading, {} dropped to save memory", l.loaded(), l.pending(), l.evicted()));
                    let (freed, bytes, count) = l.texture_stats();
                    ui.small(format!("textures: {count} on the GPU ({} MB), {freed} freed when no model used them, {} from HD dictionaries", bytes / 1_000_000, crate::world_assets::HD_TEXTURES.load(std::sync::atomic::Ordering::Relaxed)));
                    let (reduced, upgraded) = l.upgrade_stats();
                    ui.small(format!("{reduced} models have pictures cut for memory, {upgraded} loaded again at full size when the camera came close"));
                }
                if let LoadState::Ready(l) = &self.state {
                    let inside = l.stream_boxes.iter().filter(|(lo, hi)| self.cam.pos.x >= lo[0] && self.cam.pos.x <= hi[0] && self.cam.pos.y >= lo[1] && self.cam.pos.y <= hi[1]).count();
                    ui.small(format!("{inside} maps stream in at the camera (of {})", l.stream_boxes.len()));
                    ui.small(format!("{} map files, {} skipped", l.data.maps.len(), l.data.skipped.len()));
                }
            });
            ui.add(egui::Slider::new(&mut self.lod_mult, 0.2..=3.0).logarithmic(true).text("Detail distance")).on_hover_text("Multiplies every LOD distance: higher shows detailed models from further away");
            ui.add(egui::Slider::new(&mut self.view_dist, 300.0..=12000.0).logarithmic(true).suffix(" m").text("View distance")).on_hover_text("Shorter is smoother");
            ui.label("Draw models as");
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.look, Look::Textures, "Textures").on_hover_text("The textured models");
                ui.selectable_value(&mut self.look, Look::Collision, "Collision only").on_hover_text("Only the collision meshes, coloured by surface material");
                ui.selectable_value(&mut self.look, Look::Both, "Both").on_hover_text("The textured models with the collision drawn over them, see-through");
            });
            ui.horizontal(|ui| {
                ui.label("Shading");
                let names = ["Default", "Texture only", "Vertex normals", "Tangents", "Vertex AO", "Texture coords", "LOD level colours", "Vertex colour", "Vertex alpha"];
                egui::ComboBox::from_id_salt("debug_view").selected_text(names[self.debug_view as usize % names.len()]).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.debug_view, i as u32, *n);
                    }
                })
                .response
                .on_hover_text("Debug views of the models. Only the opaque models change.");
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.show_bounds, "Entity boxes").on_hover_text("The box of every drawn entity (first 1500)");
                ui.checkbox(&mut self.show_grid, "Spatial grid").on_hover_text("The cells the world is cut into for visibility, around the camera");
                ui.checkbox(&mut self.show_streaming, "Streaming regions").on_hover_text("The streaming extents of the maps the game would have loaded here: it loads a map while the camera is inside its box");
                ui.checkbox(&mut self.show_skeletons, "Skeletons").on_hover_text("The bones (joined to their parents) of the drawn models within 150 m; most props have no skeleton");
                ui.checkbox(&mut self.show_watermap, "Water map").on_hover_text("The game's rivers, lakes and pools (waterheight.dat)");
                ui.checkbox(&mut self.show_wave_quads, "Wave quads").on_hover_text("Water calming quads (blue: waves are damped) and wave quads (orange, with the wave direction), from water.xml");
                ui.checkbox(&mut self.show_audio, "Audio zones").on_hover_text("Ambient sound zones from the game's audio files: green where they start, teal where the sound is placed");
                ui.checkbox(&mut self.show_scenario, "Scenario points").on_hover_text("Where pedestrians and vehicles use scenarios (sit, smoke, work...): a post per point with an arrow for the direction it faces");
                ui.checkbox(&mut self.show_scenario_chains, "Scenario chains").on_hover_text("Links between scenario points that are used in sequence");
                ui.checkbox(&mut self.show_trains, "Train tracks").on_hover_text("The game's train tracks (trains*.dat)");
                ui.checkbox(&mut self.show_popzones, "Population zones").on_hover_text("Where the game spawns which pedestrians and cars (popzone.ipl), drawn as boxes with their names");
                ui.checkbox(&mut self.show_heightmap, "Heightmap").on_hover_text("The highest ground the game's world heightmap holds, as a grid");
                ui.checkbox(&mut self.animate_water, "Animate water").on_hover_text("Move the waves. The picture is made again about 15 times a second while this is on, which costs frame time");
                ui.checkbox(&mut self.show_water, "Water").on_hover_text("The game's water quads (water.xml): the sea, rivers, lakes and pools");
                ui.checkbox(&mut self.show_paths, "Paths").on_hover_text("Path node links (vehicle and ped paths) within 600 m, from the game's nodes files");
                ui.checkbox(&mut self.show_nav, "Nav meshes").on_hover_text("Navigation mesh polygon edges within 600 m, from the game's navmesh files");
            });
            if self.look != Look::Textures {
                ui.horizontal(|ui| {
                    ui.label("Layers");
                    for (k, on) in self.coll_layers.iter_mut().enumerate() {
                        ui.checkbox(on, format!("{k}"));
                    }
                })
                .response
                .on_hover_text("The game files its collision under three layers (from its cache file); each can be shown or hidden");
                ui.checkbox(&mut self.coll_interiors, "Interior collision").on_hover_text("The collision of placed interiors (MLOs), at the interior's position");
                ui.checkbox(&mut self.coll_wire, "Collision wireframe").on_hover_text("Draw the collision as lines instead of surfaces");
                ui.add(egui::Slider::new(&mut self.coll_range, 50.0..=2000.0).logarithmic(true).suffix(" m").text("Collision range")).on_hover_text("How far from the camera collision is drawn; a long range loads many files");

                ui.horizontal(|ui| {
                    if ui.button("Inspect under cursor").on_hover_text("Point the mouse at the scene, then press C (or this button after pointing): shows the collision surface there").clicked() {
                        self.inspect_collision();
                    }
                    if self.coll_hit.is_some() && ui.small_button("Clear").clicked() {
                        self.coll_hit = None;
                        self.coll_hit_model = None;
                        self.scene_rev = self.scene_rev.wrapping_add(1);
                    }
                });
                match &self.coll_hit {
                    Some(h) => {
                        let [r, g, b] = rage_toolkit_core::worldextra::material_color(h.material);
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
                            ui.label(format!("material {} in {}{}", h.material, h.file, h.layer.map_or(String::new(), |l| format!(" · layer {l}"))));
                        });
                        ui.small(format!("hit at {:.1}, {:.1}, {:.1} · {:.1} m away · normal {:.2}, {:.2}, {:.2}", h.point.x, h.point.y, h.point.z, h.dist, h.normal.x, h.normal.y, h.normal.z));
                        ui.small(format!("triangle {:.1},{:.1},{:.1} / {:.1},{:.1},{:.1} / {:.1},{:.1},{:.1}", h.tri[0][0], h.tri[0][1], h.tri[0][2], h.tri[1][0], h.tri[1][1], h.tri[1][2], h.tri[2][0], h.tri[2][1], h.tri[2][2]));
                        ui.small(format!("file: {} triangles, box {:.0},{:.0},{:.0} to {:.0},{:.0},{:.0}", h.file_tris, h.file_min[0], h.file_min[1], h.file_min[2], h.file_max[0], h.file_max[1], h.file_max[2]));
                        let top: Vec<String> = h.materials.iter().take(6).map(|(m, n)| format!("{m}: {n}")).collect();
                        ui.small(format!("materials in the file (id: triangles): {}", top.join(", ")));
                    }
                    None => {
                        ui.weak("Press C with the mouse over the scene to inspect a collision surface.");
                    }
                }
            }
            if let LoadState::Ready(l) = &self.state {
                if !l.dlcs.is_empty() {
                    let names: Vec<String> = l.dlcs.iter().map(|d| d.name.clone()).collect();
                    let mut pick = self.dlc_level;
                    ui.horizontal(|ui| {
                        ui.label("DLC level");
                        egui::ComboBox::from_id_salt("dlc_level").selected_text(pick.map_or("Latest (everything)".to_string(), |i| names[i.min(names.len() - 1)].clone())).show_ui(ui, |ui| {
                            ui.selectable_value(&mut pick, None, "Latest (everything)");
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut pick, Some(i), n.as_str());
                            }
                        })
                        .response
                        .on_hover_text("The game as it was up to this DLC pack: maps of later packs are not drawn, and an area that a pack up to it replaces with its own maps (the heists' and lowriders' hei_ and lr_ maps) shows the newest of those. A map a later pack changes under the same name shows its newest version.");
                    });
                    if pick != self.dlc_level {
                        self.dlc_level = pick;
                        self.rebuild_hidden();
                        self.bump();
                    }
                }
            }
            ui.add(egui::Slider::new(&mut self.hour, 0.0..=24.0).suffix(" h").text("Time of day")).on_hover_text("Moves the sun and darkens the sky and the light");
            ui.checkbox(&mut self.hdr_light, "HDR lighting").on_hover_text("Light, sky and tone mapping in HDR: the timecycle's light in HDR units, an exposure, no gamma. Off: the older shading");
            ui.checkbox(&mut self.fog, "Distance fog").on_hover_text("Fade distant things to the sky with the timecycle's fog (off by default)");
            ui.checkbox(&mut self.linear_light, "Linear light").on_hover_text("Shade in linear light and tone-map, as the game does; off is the older shading on display values");
            ui.horizontal(|ui| {
                ui.label("Weather");
                let names = self.weather_names();
                egui::ComboBox::from_id_salt("weather").selected_text(names[self.weather as usize % names.len()].clone()).show_ui(ui, |ui| {
                    for (i, w) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.weather, i as u8, w);
                    }
                })
                .response
                .on_hover_text("The game's timecycle for this weather and hour: sun, ambient and fog colours, and the sky. Clouds are one procedural layer with a cover chosen per weather.");
            });
            ui.checkbox(&mut self.shadows, "Shadows (experimental)").on_hover_text("The sun casts shadows within about 140 m of the camera. Costs frame time.");
            ui.checkbox(&mut self.show_extents, "Show the active map's extents").on_hover_text("Green: the entity extents. Orange: the streaming extents. Solid: stored in the file. Dashed look: calculated from the entities.");
        });
    }

    fn find_section(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(small_heading("Find a model")).default_open(false).show(ui, |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Name, e.g. prop_chair").desired_width(f32::INFINITY));
            if r.changed() {
                self.search();
            }
            if self.query.trim().is_empty() {
                ui.weak("Lists every place it is used, nearest to you first.");
            } else {
                ui.weak(format!("{} placements{}", self.results.len(), if self.results.len() >= MAX_RESULTS { " (first 300)" } else { "" }));
            }
            let rows = self.results.clone();
            let mut picked: Option<u32> = None;
            egui::ScrollArea::vertical().id_salt("world_results").max_height(200.0).auto_shrink([false, true]).show_rows(ui, 22.0, rows.len(), |ui, range| {
                for &i in &rows[range] {
                    let Some((name, map)) = (match &self.state {
                        LoadState::Ready(l) => l.data.ents.get(i as usize).map(|e| (self.name(e.hash), leaf(&l.data.maps[e.map as usize]).to_string())),
                        _ => None,
                    }) else {
                        continue;
                    };
                    if ui.selectable_label(self.is_selected(i), format!("{name}   {map}")).clicked() {
                        picked = Some(i);
                    }
                }
            });
            if let Some(i) = picked {
                self.select(i, true);
            }
            ui.separator();
            ui.label("Go to");
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.goto_text).hint_text("x y z").desired_width(170.0));
                if (ui.button("Go").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && !self.goto_text.trim().is_empty() {
                    let n: Vec<f32> = self.goto_text.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|t| t.parse().ok()).collect();
                    if n.len() >= 2 {
                        self.cam.pos = Vec3::new(n[0], n[1], n.get(2).copied().unwrap_or(150.0));
                    }
                }
            });
            ui.label("Bookmarks");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.bookmark_name).hint_text("name").desired_width(120.0));
                if ui.button("Save here").on_hover_text("Remember the camera's place and direction").clicked() {
                    let name = if self.bookmark_name.trim().is_empty() { format!("{:.0}, {:.0}, {:.0}", self.cam.pos.x, self.cam.pos.y, self.cam.pos.z) } else { self.bookmark_name.trim().to_string() };
                    self.bookmarks.push((name, [self.cam.pos.x, self.cam.pos.y, self.cam.pos.z, self.cam.yaw, self.cam.pitch]));
                    self.bookmark_name.clear();
                    save_bookmarks(&self.bookmarks);
                }
            });
            let mut remove = None;
            let mut go = None;
            for (i, (name, _)) in self.bookmarks.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.link(name).clicked() {
                        go = Some(i);
                    }
                    if ui.small_button("x").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = go {
                let v = self.bookmarks[i].1;
                self.cam.pos = Vec3::new(v[0], v[1], v[2]);
                self.cam.yaw = v[3];
                self.cam.pitch = v[4];
            }
            if let Some(i) = remove {
                self.bookmarks.remove(i);
                save_bookmarks(&self.bookmarks);
            }
        });
    }

    fn add_section(&mut self, ui: &mut egui::Ui) {
        // the toolbar's Add button opens this section
        if std::mem::take(&mut self.open_add) {
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id("add_model"), false);
            state.set_open(true);
            state.store(ui.ctx());
        }
        egui::CollapsingHeader::new(small_heading("Add a model")).id_salt("add_model").default_open(false).show(ui, |ui| {
            let target = self.proj.active_map().map(|m| m.name.clone());
            ui.weak(match &target {
                Some(n) => format!("It goes into {n}, 25 m in front of the camera."),
                None => "There is no map yet: one is made for it.".into(),
            });
            let field = ui.add(egui::TextEdit::singleline(&mut self.add_query).hint_text("Name, e.g. prop_bench_01a").desired_width(f32::INFINITY));
            if std::mem::take(&mut self.focus_add) {
                field.request_focus();
            }
            ui.horizontal(|ui| {
                if ui.button("Model folder...").on_hover_text("Use the .ydr, .yft, .ydd and .ytd files in a folder (and its .ytyp files) of yours: props named like a file there are drawn with that model").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Folder with your models").pick_folder() {
                        self.load_model_folder(&dir);
                    }
                }
                if ui.button("Load .ytyp...").on_hover_text("Make the archetypes of a .ytyp of yours known: their sizes are used for boxes, picking and extents. Type a name below to place one.").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("Archetype file", &["ytyp"]).pick_file() {
                        self.project_msg = match std::fs::read(&f).map_err(anyhow::Error::from).and_then(|b| rage_toolkit_core::worldload::archetypes_of_ytyp(&b)) {
                            Ok(list) => {
                                let n = list.len();
                                rage_toolkit_core::worldload::add_custom_archetypes(list);
                                self.bump();
                                format!("{n} archetypes from {} are known now.", f.file_name().unwrap_or_default().to_string_lossy())
                            }
                            Err(e) => format!("{e:#}"),
                        };
                    }
                }
                let custom = rage_toolkit_core::worldload::custom_archetype_count();
                if custom > 0 && ui.small_button(format!("Forget {custom}")).on_hover_text("Forget the archetypes loaded from .ytyp files").clicked() {
                    rage_toolkit_core::worldload::clear_custom_archetypes();
                    self.bump();
                }
            });
            if self.add_query.trim().len() >= 2 && ui.button(format!("Place \"{}\"", self.add_query.trim())).on_hover_text("Place a model by exactly this name, even when no game file lists it (your own prop)").clicked() {
                let p = self.cam.pos + self.cam.forward() * 25.0;
                let n = self.add_query.trim().to_string();
                self.add_entity(&n, EntityEdit { pos: p.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
            }
            let q = self.add_query.trim().to_lowercase();
            if q.len() >= 2 {
                let names: Vec<String> = match &self.state {
                    LoadState::Ready(l) => l.arch_names.iter().filter(|n| n.to_lowercase().contains(&q)).take(8).cloned().collect(),
                    _ => vec![],
                };
                for n in names {
                    if ui.selectable_label(false, n.as_str()).on_hover_text("Place it").clicked() {
                        let p = self.cam.pos + self.cam.forward() * 25.0;
                        self.add_entity(&n, EntityEdit { pos: p.to_array(), rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
                    }
                }
            }
        });
    }

    fn changes_section(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(small_heading("Changes and export")).default_open(true).show(ui, |ui| {
            let moved = self.edits.len();
            let any_game = !self.edits.is_empty() || !self.deleted.is_empty();
            if any_game {
                ui.label(format!("Game maps: {moved} moved, {} removed", self.deleted.len())).on_hover_text("Changes to the game's own entities. They are written into copies of those maps when you export.");
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Ctrl+Z").on_disabled_hover_text("Nothing to undo").clicked() {
                    self.undo_step();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo")).on_hover_text("Ctrl+Y").on_disabled_hover_text("Nothing to redo").clicked() {
                    self.redo_step();
                }
            });
            let can_export = (any_game || !self.proj.maps.is_empty()) && self.game.is_some();
            let export = egui::Button::new(egui::RichText::new("Export maps...").strong()).fill(crate::theme::accent().gamma_multiply(0.45)).min_size(egui::vec2(ui.available_width(), 30.0));
            if ui.add_enabled(can_export, export).on_hover_text("Write the project's maps and the changed game maps to a folder you choose. The game's own files are never changed.").on_disabled_hover_text("Make or import a map, or change something, first").clicked() {
                self.export();
            }
            egui::CollapsingHeader::new("Export options").default_open(false).show(ui, |ui| {
                ui.checkbox(&mut self.recalc_extents, "Recalculate extents from scratch").on_hover_text("Off: the extents of a copy of a game map only grow where an entity now reaches past them. On: they are worked out again from every entity.");
                ui.checkbox(&mut self.auto_update, "Work out flags and extents when saving").on_hover_text("Recalculate the flags and extents of a changed map before saving, so the game gets consistent values.");
            });
            if any_game {
                ui.add_space(8.0);
                if ui.add(egui::Button::new(danger_text("Reset changes to game entities...")).small()).on_hover_text("Put the game's entities back as the game has them (your project's maps are kept)").clicked() {
                    let sure = rfd::MessageDialog::new().set_title("Reset").set_description("Put every moved game entity back and bring back the removed ones? This cannot be undone.").set_buttons(rfd::MessageButtons::YesNo).show();
                    if sure == rfd::MessageDialogResult::Yes {
                        self.reset_game_changes();
                    }
                }
            }
        });
    }

    // ---- the right panel -------------------------------------------------------------------------------------------------------

    pub(super) fn right_panel(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        if !self.proj.maps.is_empty() {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.right_tab, RightTab::Entity, "Entity");
                ui.selectable_value(&mut self.right_tab, RightTab::Map, "Map").on_hover_text("The active map's name, flags, extents and checks");
            });
            ui.separator();
        } else {
            self.right_tab = RightTab::Entity;
        }
        egui::ScrollArea::vertical().id_salt("world_right_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            match self.right_tab {
                RightTab::Entity => self.entity_panel(ui),
                RightTab::Map => self.map_panel(ui),
            }
        });
    }

    fn entity_panel(&mut self, ui: &mut egui::Ui) {
        let LoadState::Ready(_) = &self.state else {
            ui.weak("The map is still loading.");
            return;
        };
        self.prune_selection();
        let ids = self.sel.clone();
        if ids.is_empty() {
            ui.heading("Nothing selected");
            if self.mode == ViewMode::Camera {
                ui.label("You are in Camera mode: the mouse only moves the view.");
                ui.label("Press Tab, or use the Select button above the view, to pick things.");
            } else {
                ui.label("Click something in the view, drag a box round several, or pick a model from \"Find a model\".");
                ui.weak("Ctrl or Shift adds to the selection.");
            }
            return;
        }
        if ids.len() > 1 {
            self.group_panel(ui, &ids);
            return;
        }
        let id = ids[0];
        let Some(e) = self.ent_of(id) else { return };
        let is_proj = id >= projmod::BASE;
        let name = self.name(e.hash);
        let place = self.placement_of(id).unwrap_or_else(|| EntityEdit::of(&e));
        ui.label(egui::RichText::new(&name).strong().size(18.0));
        let where_text = match projmod::split(id) {
            Some((slot, index)) => format!("{} · entity {index}", self.proj.maps.get(slot).map_or(String::new(), |m| m.name.clone())),
            None => match &self.state {
                LoadState::Ready(l) => format!("{} · entity {}", leaf(&l.data.maps[e.map as usize]), e.index),
                _ => String::new(),
            },
        };
        ui.weak(where_text);
        self.entity_actions(ui, id, &name, place);
        ui.separator();
        // ---- placement
        let mut cur = place;
        let mut changed = false;
        egui::CollapsingHeader::new(small_heading("Placement")).default_open(true).show(ui, |ui| {
            if !is_proj && self.edits.contains_key(&id) {
                ui.label(egui::RichText::new("changed from the game").color(crate::theme::warn()));
            }
            egui::Grid::new("world_place").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    for v in cur.pos.iter_mut() {
                        changed |= ui.add(egui::DragValue::new(v).speed(0.05).max_decimals(3)).changed();
                    }
                });
                ui.end_row();
                // the rotation as the game's tools show it: heading, pitch, roll in degrees (the file stores the inverse quaternion)
                let (yaw, pitch, roll) = gizmo::world_rot(cur.rot).to_euler(glam::EulerRot::ZYX);
                let mut deg = [yaw.to_degrees(), pitch.to_degrees(), roll.to_degrees()];
                ui.label("Rotation");
                let mut rot_changed = false;
                ui.horizontal(|ui| {
                    for v in deg.iter_mut() {
                        rot_changed |= ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2).suffix("°")).changed();
                    }
                });
                ui.end_row();
                if rot_changed {
                    cur.rot = gizmo::stored_rot(Quat::from_euler(glam::EulerRot::ZYX, deg[0].to_radians(), deg[1].to_radians(), deg[2].to_radians()));
                    changed = true;
                }
                ui.label("Scale");
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(&mut cur.scale_xy).speed(0.01).range(0.01..=1000.0).max_decimals(3)).changed();
                    changed |= ui.add(egui::DragValue::new(&mut cur.scale_z).speed(0.01).range(0.01..=1000.0).max_decimals(3)).changed();
                });
                ui.end_row();
            });
            self.vertical_buttons(ui, &[id]);
            if !is_proj {
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.edits.contains_key(&id), egui::Button::new("Reset this")).on_hover_text("Put it back where the game has it").on_disabled_hover_text("It has not been changed").clicked() {
                        self.reset_selection();
                    }
                });
            }
        });
        if changed {
            self.set_placements(&[(id, cur)]);
        }
        if is_proj {
            self.lod_section(ui, id);
            self.members_section(ui, id);
            self.extensions_section(ui, id);
        } else {
            self.game_entity_notes(ui, id);
        }
        self.model_section(ui, id, &e);
    }

    /// Duplicate, copy, delete for one entity.
    fn entity_actions(&mut self, ui: &mut egui::Ui, id: u32, name: &str, place: EntityEdit) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Duplicate").on_hover_text("A copy 2 m to the side, in the active map (Ctrl+D). Shift-drag a handle copies too.").clicked() {
                self.duplicate_selection(Vec3::new(2.0, 0.0, 0.0));
            }
            ui.menu_button("Copy", |ui| {
                if ui.button("Name").clicked() {
                    ui.ctx().copy_text(name.to_string());
                    ui.close();
                }
                if ui.button("x, y, z").clicked() {
                    ui.ctx().copy_text(format!("{:.3}, {:.3}, {:.3}", place.pos[0], place.pos[1], place.pos[2]));
                    ui.close();
                }
                if ui.button("vector3(x, y, z)").on_hover_text("For a FiveM script").clicked() {
                    ui.ctx().copy_text(format!("vector3({:.3}, {:.3}, {:.3})", place.pos[0], place.pos[1], place.pos[2]));
                    ui.close();
                }
                if ui.button("vector4(x, y, z, heading)").on_hover_text("The heading is in degrees").clicked() {
                    let heading = gizmo::world_rot(place.rot).to_euler(glam::EulerRot::ZYX).0.to_degrees();
                    ui.ctx().copy_text(format!("vector4({:.3}, {:.3}, {:.3}, {:.3})", place.pos[0], place.pos[1], place.pos[2], heading));
                    ui.close();
                }
            });
            ui.add_space(12.0);
            if ui.button(danger_text("Delete")).on_hover_text("Take it out of the map (Delete key). Undo brings it back.").clicked() {
                self.delete_selection();
            }
        });
        let _ = id;
    }

    /// raise and lower buttons and the snap-to-ground commands for the selection
    fn vertical_buttons(&mut self, ui: &mut egui::Ui, ids: &[u32]) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Height");
            for (label, dz) in [("-1", -1.0f32), ("-0.1", -0.1), ("+0.1", 0.1), ("+1", 1.0)] {
                if ui.small_button(label).on_hover_text("Raise or lower by this many metres. Nothing stops an entity going underground.").clicked() {
                    self.nudge(ids, Vec3::new(0.0, 0.0, dz));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Snap to ground").on_hover_text("Drop it onto the nearest surface below it (the game's collision, terrain included). It stays where it is when there is none.").clicked() {
                self.snap_selection_to_ground(GroundMode::Below, self.auto_align);
            }
            if ui.button("Onto the highest surface").on_hover_text("Put it on the topmost surface at its x and y (a roof, a bridge)").clicked() {
                self.snap_selection_to_ground(GroundMode::Top, self.auto_align);
            }
            ui.checkbox(&mut self.auto_align, "Lie along the surface").on_hover_text("When snapping, also turn it to follow the slope (its heading is kept)");
        });
    }

    /// several entities selected
    fn group_panel(&mut self, ui: &mut egui::Ui, ids: &[u32]) {
        ui.heading(format!("{} entities", ids.len()));
        let in_proj = ids.iter().filter(|i| **i >= projmod::BASE).count();
        ui.weak(format!("{in_proj} in project maps, {} from the game", ids.len() - in_proj));
        ui.horizontal_wrapped(|ui| {
            if ui.button("Duplicate").on_hover_text("Copies 2 m to the side, in the active map (Ctrl+D)").clicked() {
                self.duplicate_selection(Vec3::new(2.0, 0.0, 0.0));
            }
            if ui.button("Copy").on_hover_text("Ctrl+C").clicked() {
                self.copy_selection();
            }
            if ui.add_enabled(!self.clipboard.is_empty(), egui::Button::new("Paste")).on_hover_text("Ctrl+V: in front of the camera").clicked() {
                let at = self.cam.pos + self.cam.forward() * 25.0;
                self.paste(at);
            }
            ui.add_space(12.0);
            if ui.button(danger_text("Delete")).on_hover_text("Delete key. Undo brings them back.").clicked() {
                self.delete_selection();
            }
        });
        ui.separator();
        ui.label("Drag the handles in the view to move, turn or scale them together about their middle.");
        self.vertical_buttons(ui, ids);
        // LOD: for entities of one project map
        let slots: HashSet<usize> = ids.iter().filter_map(|i| projmod::split(*i).map(|s| s.0)).collect();
        if slots.len() == 1 && in_proj == ids.len() {
            let slot = *slots.iter().next().unwrap();
            let indices: Vec<usize> = ids.iter().filter_map(|i| projmod::split(*i).map(|s| s.1)).collect();
            let mut level: Option<u8> = None;
            let (mut choose, mut remove) = (false, false);
            egui::CollapsingHeader::new(small_heading("LOD for all of them")).default_open(true).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Level");
                    egui::ComboBox::from_id_salt("group_level").selected_text("set to...").show_ui(ui, |ui| {
                        for l in [0u8, 1, 2, 3, 4, 6, 5] {
                            if ui.selectable_label(false, lod_name(l)).clicked() {
                                level = Some(l);
                            }
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    choose = ui.button("Choose parent...").on_hover_text("Then click the parent in the view or in the list: all of them become its LOD children").clicked();
                    remove = ui.button("Remove parents").on_hover_text("They all become roots").clicked();
                });
                if self.pick_parent.as_deref() == Some(ids) {
                    ui.label(egui::RichText::new("Click the parent in the view or in the list. Esc cancels.").color(crate::theme::accent()));
                }
            });
            if let Some(l) = level {
                let list: Vec<(usize, u8)> = indices.iter().map(|&i| (i, l)).collect();
                if self.doc_op(slot, |d| d.set_lod_levels(&list)).is_some() {
                    self.project_msg = format!("{} entities are {} now.", list.len(), lod_name(l));
                    self.reveal_level(l);
                }
                self.end_groups();
            }
            if choose {
                self.pick_parent = Some(ids.to_vec());
            }
            if remove {
                self.pick_parent = None;
                self.assign_parent(ids, None);
            }
        }
        if ui.button("Select none").clicked() {
            self.set_selection(vec![]);
        }
    }

    fn game_entity_notes(&mut self, ui: &mut egui::Ui, id: u32) {
        let Some(e) = self.ent_of(id) else { return };
        let (map_name, map_count) = match &self.state {
            LoadState::Ready(l) => (leaf(&l.data.maps[e.map as usize]).to_string(), l.data.ents.iter().filter(|x| x.map == e.map).count()),
            _ => (String::new(), 0),
        };
        egui::CollapsingHeader::new(small_heading("Its YMAP")).default_open(true).show(ui, |ui| {
            ui.label(format!("This entity comes from {map_name}, which has {map_count} entities."));
            ui.label("Add the whole YMAP to the project to edit it: every entity, flag, LOD relation, extension and extent comes with it.");
            if ui.button(egui::RichText::new("Add the YMAP to the project").strong()).on_hover_text("Makes an editable copy of the complete map in the project. Your changes to its entities so far move into the copy.").clicked() {
                self.import_game_map(e.map);
            }
        });
    }

    fn model_section(&mut self, ui: &mut egui::Ui, id: u32, e: &WorldEnt) {
        let LoadState::Ready(l) = &self.state else { return };
        let data = l.data.clone();
        egui::CollapsingHeader::new(small_heading("Model")).default_open(false).show(ui, |ui| {
            match data.arch_of(&e.hash) {
                None => {
                    ui.weak("No .ytyp file defines this name (it may be defined by a file this tool could not read).");
                }
                Some(a) => {
                    let kind = match a.asset_type {
                        1 => "fragment (.yft)".to_string(),
                        2 => "drawable (.ydr)".to_string(),
                        3 => format!("in the drawable dictionary {}.ydd", self.name(a.ydd_hash)),
                        4 => "no model".to_string(),
                        _ => "unknown".to_string(),
                    };
                    let size = Vec3::from(a.bb_max) - Vec3::from(a.bb_min);
                    egui::Grid::new("world_arch").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Kind");
                        ui.add(egui::Label::new(kind).wrap());
                        ui.end_row();
                        ui.label("Size");
                        ui.label(format!("{:.1} x {:.1} x {:.1} m", size.x, size.y, size.z));
                        ui.end_row();
                        ui.label("Defined in");
                        ui.add(egui::Label::new(leaf(&data.ytyps[a.ytyp as usize]).to_string()).wrap());
                        ui.end_row();
                    });
                }
            }
            let count = match self.placements.get() {
                Some((h, n)) if h == e.hash => n,
                _ => {
                    let n = data.ents.iter().filter(|x| x.hash == e.hash).count();
                    self.placements.set(Some((e.hash, n)));
                    n
                }
            };
            ui.label(format!("Placed {count} time{} in the game's maps", if count == 1 { "" } else { "s" }));
            match &self.model {
                Some((j, Some(path))) if *j == id => {
                    if ui.button("Open the model in the Viewer").on_hover_text(path.as_str()).clicked() {
                        self.open_model_request = Some(path.clone());
                    }
                }
                Some((j, None)) if *j == id => {
                    ui.weak("The model file was not found in the game's archives.");
                }
                _ => {}
            }
            ui.label(format!("Name hash 0x{:08X}", e.hash));
        });
    }

    // ---- LOD hierarchy ---------------------------------------------------------------------------------------------------------

    /// Make `parent` the LOD parent of every one of `children` (`None`: they become roots), one undo step per map. The parent has to be in the
    /// same map as the children; one that cannot be (a loop, a level that is not higher) is refused and says why.
    pub(super) fn assign_parent(&mut self, children: &[u32], parent: Option<u32>) {
        let target = match parent {
            Some(p) => match projmod::split(p) {
                Some(x) => Some(x),
                None => {
                    self.project_msg = "The parent has to be an entity of a project map.".into();
                    return;
                }
            },
            None => None,
        };
        let mut by_slot: BTreeMap<usize, Vec<(usize, Option<usize>)>> = BTreeMap::new();
        for &c in children {
            let Some((slot, index)) = projmod::split(c) else { continue };
            match target {
                Some((ps, _)) if ps != slot => {
                    self.project_msg = "The parent has to be in the same map as the entities.".into();
                    return;
                }
                Some((_, pi)) if pi == index => {}
                Some((_, pi)) => by_slot.entry(slot).or_default().push((index, Some(pi))),
                None => by_slot.entry(slot).or_default().push((index, None)),
            }
        }
        let mut done = 0;
        for (slot, list) in by_slot {
            if self.doc_op(slot, |d| d.set_parents(&list)).is_some() {
                done += list.len();
            }
        }
        if done > 0 {
            let (y, are) = if done == 1 { ("y", "is a root") } else { ("ies", "are roots") };
            self.project_msg = if parent.is_some() { format!("LOD parent set for {done} entit{y}.") } else { format!("{done} entit{y} {are} now.") };
            self.end_groups();
        }
    }

    fn lod_section(&mut self, ui: &mut egui::Ui, id: u32) {
        let Some((slot, index)) = projmod::split(id) else { return };
        let (parent, children, ancestors, my_issues) = {
            let hr = self.hier_rows(slot);
            let h = &hr.h;
            (
                h.parent.get(index).copied().flatten(),
                h.children.get(index).cloned().unwrap_or_default(),
                h.ancestors(index),
                h.issues.iter().filter(|i| i.entity == index).map(|i| (i.kind, i.message.clone())).collect::<Vec<(LodIssueKind, String)>>(),
            )
        };
        let Some(m) = self.proj.maps.get(slot) else { return };
        let Some(e) = m.doc.entity(index) else { return };
        let in_parent_map = e.flags & (1 << 3) != 0;
        let names: Vec<(usize, String)> = children.iter().chain(parent.iter()).map(|&i| (i, m.doc.entity(i).map(|x| self.name(x.archetype_hash)).unwrap_or_default())).collect();
        let nm = |i: usize| names.iter().find(|(j, _)| *j == i).map(|(_, n)| n.clone()).unwrap_or_default();
        let mut level = e.lod_level;
        let (mut lod_dist, mut child_dist) = (e.lod_dist, e.child_lod_dist);
        let (mut lod_changed, mut child_changed, mut level_changed) = (false, false, false);
        let mut act_parent: Option<Option<u32>> = None;
        let mut go: Option<u32> = None;
        let external = (e.num_children as usize).saturating_sub(children.len());
        egui::CollapsingHeader::new(small_heading("LOD hierarchy")).default_open(true).show(ui, |ui| {
            let role = if parent.is_some() { "a LOD child" } else if in_parent_map { "a root here: its parent is in the parent map" } else if !children.is_empty() { "a root with children" } else { "a root" };
            ui.label(egui::RichText::new(role).strong());
            for (kind, msg) in &my_issues {
                let (icon, col) = if *kind == LodIssueKind::ParentInOtherMap { ("Note:", crate::theme::dim()) } else { ("Problem:", crate::theme::warn()) };
                ui.label(egui::RichText::new(format!("{icon} {msg}")).small().color(col));
            }
            egui::Grid::new("world_lod").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Level");
                egui::ComboBox::from_id_salt("lod_level").selected_text(lod_name(level)).show_ui(ui, |ui| {
                    for l in [0u8, 1, 2, 3, 4, 6, 5] {
                        if ui.selectable_value(&mut level, l, lod_name(l)).clicked() {
                            level_changed = true;
                        }
                    }
                });
                ui.end_row();
                ui.label("LOD distance");
                lod_changed = ui.add(egui::DragValue::new(&mut lod_dist).speed(1.0).suffix(" m")).on_hover_text("How far away it is shown. 0 or less uses the model's own distance.").changed();
                ui.end_row();
                ui.label("Child LOD distance");
                child_changed = ui.add(egui::DragValue::new(&mut child_dist).speed(1.0).suffix(" m")).on_hover_text("Closer than this its LOD children are drawn instead of it. 0 or less: half its LOD distance.").changed();
                ui.end_row();
            });
            ui.separator();
            match parent {
                Some(p) => {
                    ui.horizontal(|ui| {
                        ui.label(format!("Parent: {} (entity {p})", nm(p)));
                        if ui.small_button("Select").clicked() {
                            go = Some(projmod::id(slot, p));
                        }
                    });
                    if ancestors.len() > 1 {
                        ui.weak(format!("{} levels above it", ancestors.len()));
                    }
                }
                None => {
                    ui.label(if in_parent_map { format!("Parent: entity {} of the parent map", e.parent_index) } else { "Parent: none".to_string() });
                }
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Choose parent...").on_hover_text("Then click the parent in the view or in the entity list. It must be of a higher LOD level, in this map.").clicked() {
                    act_parent = Some(Some(u32::MAX));
                }
                if ui.add_enabled(parent.is_some() || in_parent_map, egui::Button::new("Remove parent")).on_disabled_hover_text("It has no parent").clicked() {
                    act_parent = Some(None);
                }
            });
            if self.pick_parent.as_deref() == Some(&[id][..]) {
                ui.label(egui::RichText::new("Click the parent in the view or in the list. Esc cancels.").color(crate::theme::accent()));
            }
            ui.separator();
            ui.label(format!("Children in this map: {}", children.len()));
            for &c in children.iter().take(12) {
                if ui.small_button(format!("{c}  {}", nm(c))).clicked() {
                    go = Some(projmod::id(slot, c));
                }
            }
            if children.len() > 12 {
                ui.weak(format!("and {} more", children.len() - 12));
            }
            if external > 0 {
                ui.weak(format!("{external} more are in other maps (the entity says it has {})", e.num_children));
            }
        });
        if let Some(p) = act_parent {
            match p {
                Some(_) => self.pick_parent = Some(vec![id]),
                None => {
                    self.pick_parent = None;
                    self.assign_parent(&[id], None);
                }
            }
        }
        if level_changed {
            if self.doc_op(slot, |d| d.set_lod_level(index, level)).is_some() {
                self.reveal_level(level);
            }
        }
        if lod_changed {
            self.doc_op(slot, |d| d.set_lod_dist(index, lod_dist));
        }
        if child_changed {
            self.doc_op(slot, |d| d.set_child_lod_dist(index, child_dist));
        }
        if let Some(g) = go {
            self.select(g, true);
        }
    }

    /// the other members of a project entity, as text boxes: applied when the box loses focus
    fn members_section(&mut self, ui: &mut egui::Ui, id: u32) {
        let Some((slot, index)) = projmod::split(id) else { return };
        let Some(m) = self.proj.maps.get(slot) else { return };
        let rows = m.doc.entity_fields(index);
        let arch = m.doc.entity(index).map(|e| self.name(e.archetype_hash)).unwrap_or_default();
        let skip = ["position", "rotation", "scaleXY", "scaleZ", "parentIndex", "lodLevel", "lodDist", "childLodDist", "numChildren", "archetypeName"];
        let mut apply: Vec<(String, String)> = vec![];
        let mut new_arch: Option<String> = None;
        egui::CollapsingHeader::new(small_heading("Properties")).default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Archetype");
                let key = format!("{id}:archetype");
                let buf = self.field_bufs.entry(key).or_insert_with(|| arch.clone());
                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(f32::INFINITY));
                if r.lost_focus() && buf.trim() != arch && !buf.trim().is_empty() {
                    new_arch = Some(buf.trim().to_string());
                }
            });
            egui::Grid::new("world_members").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for r in rows.iter().filter(|r| !skip.contains(&r.name.as_str())) {
                    ui.label(&r.name);
                    let key = format!("{id}:{}", r.name);
                    let buf = self.field_bufs.entry(key).or_insert_with(|| r.text.clone());
                    let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                    if resp.lost_focus() && *buf != r.text {
                        apply.push((r.name.clone(), buf.clone()));
                    } else if !resp.has_focus() && *buf != r.text {
                        // the file changed under it (an undo): show what is there
                        *buf = r.text.clone();
                    }
                    ui.end_row();
                }
            });
        });
        for (name, text) in apply {
            self.doc_op(slot, |d| d.set_entity_field(index, &name, &text));
            self.field_bufs.remove(&format!("{id}:{name}"));
            self.end_groups();
        }
        if let Some(a) = new_arch {
            self.doc_op(slot, |d| d.set_archetype(index, &a));
            self.extra_names.insert(rage_toolkit_core::rage_joaat(&a.to_lowercase()), a);
            self.field_bufs.remove(&format!("{id}:archetype"));
            self.end_groups();
        }
    }

    /// move the selected entity of a project map one place in the list
    fn reorder_selected(&mut self, by: i32) {
        let Some(&id) = self.sel.first() else { return };
        let Some((slot, index)) = projmod::split(id) else { return };
        let n = self.proj.maps.get(slot).map_or(0, |m| m.doc.len());
        let to = index as i32 + by;
        if to < 0 || to as usize >= n {
            return;
        }
        let impact = self.proj.maps[slot].doc.removal_impact(index.min(to as usize));
        if self.doc_op(slot, |d| d.reorder_entity(index, to as usize)).is_some() {
            self.set_selection(vec![projmod::id(slot, to as usize)]);
            if !impact.is_empty() {
                self.project_msg = format!("Moved. {} entities after it have LOD children in other maps, which name them by place: check those maps.", impact.len());
            }
        }
    }

    // ---- the map's own properties ---------------------------------------------------------------------------------------------

    fn map_panel(&mut self, ui: &mut egui::Ui) {
        let slot = self.proj.active;
        let Some(m) = self.proj.maps.get(slot) else {
            ui.weak("Make or import a map to see its properties.");
            return;
        };
        let (name, kind, file, source, count, dirty) = (m.name.clone(), m.kind, m.file.display().to_string(), m.source.clone(), m.doc.len(), m.dirty());
        let fields = m.doc.map_fields();
        let parent = m.doc.parent_name();
        let internal_name = m.doc.name();
        let stored_ex = m.doc.stored_extents();
        let calc = self.calc_extents_cached(slot);
        let flag_lines = self.flag_lines_cached(slot);
        let (smin, smax, tmin, tmax) = (stored_ex.map(|e| e.entities_min), stored_ex.map(|e| e.entities_max), stored_ex.map(|e| e.streaming_min), stored_ex.map(|e| e.streaming_max));
        let (roots, levels, lod_issues) = {
            let hr = self.hier_rows(slot);
            (hr.h.roots.len(), hr.levels, hr.h.issues.iter().map(|i| (i.entity, i.is_error(), i.message.clone())).collect::<Vec<_>>())
        };
        let mut rebuild: Option<CountMode> = None;
        let mut go_lod: Option<usize> = None;
        ui.heading(&name);
        ui.weak(format!("{} entities{}", count, if dirty { " · unsaved changes" } else { "" }));
        let mut set_field: Option<(&'static str, String)> = None;
        let mut flag_toggle: Option<(&'static str, u32, bool)> = None;
        let mut calc_flags = false;
        let mut calc_ext: Option<ExtentsMode> = None;
        let mut validate = false;
        egui::CollapsingHeader::new(small_heading("Identity")).default_open(true).show(ui, |ui| {
            egui::Grid::new("map_identity").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Name");
                let buf = self.map_bufs.entry(format!("{slot}:name")).or_insert_with(|| internal_name.clone());
                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                if r.lost_focus() && buf.trim() != internal_name && !buf.trim().is_empty() {
                    set_field = Some(("name", buf.trim().to_string()));
                }
                ui.end_row();
                ui.label("Parent map");
                let buf = self.map_bufs.entry(format!("{slot}:parent")).or_insert_with(|| parent.clone());
                let r = ui.add(egui::TextEdit::singleline(buf).hint_text("none").desired_width(150.0));
                if r.lost_focus() && buf.trim() != parent {
                    set_field = Some(("parent", buf.trim().to_string()));
                }
                ui.end_row();
                ui.label("Kind");
                ui.label(match kind {
                    MapKind::New => "made in the project",
                    MapKind::File => "imported from a file",
                    MapKind::Game => "a copy of a game map",
                });
                ui.end_row();
                if let Some(s) = &source {
                    ui.label("From");
                    ui.add(egui::Label::new(s.as_str()).wrap());
                    ui.end_row();
                }
            });
            ui.label(egui::RichText::new(format!("Working copy: {file}")).small().weak());
            ui.weak("The parent map is the one whose LOD entities these are the children of.");
        });
        egui::CollapsingHeader::new(small_heading("Flags")).default_open(true).show(ui, |ui| {
            ui.weak("A tick is what the file has. A warning says what the contents suggest.");
            for line in &flag_lines {
                ui.horizontal(|ui| {
                    let mut on = line.stored;
                    let field: &'static str = line.field;
                    if ui.checkbox(&mut on, line.name).on_hover_text(line.meaning).changed() {
                        flag_toggle = Some((field, line.bit, on));
                    }
                    if line.disagrees() {
                        ui.label(egui::RichText::new(if line.expected == Some(true) { "should be on" } else { "should be off" }).small().color(crate::theme::warn()));
                    } else if line.expected.is_none() {
                        ui.label(egui::RichText::new("kept as it is").small().weak());
                    }
                });
            }
            if ui.button("Calculate all flags").on_hover_text("Set the flags the contents decide (HD, LOD, SLOD, interior, occlusion, physics, lights, grass). Other bits, such as scripted and critical, are kept.").clicked() {
                calc_flags = true;
            }
            for f in fields.iter().filter(|f| f.name == "flags" || f.name == "contentFlags") {
                ui.label(egui::RichText::new(format!("{} = {}", f.name, f.text)).small().weak());
            }
        });
        egui::CollapsingHeader::new(small_heading("Extents")).default_open(true).show(ui, |ui| {
            egui::Grid::new("map_extents").num_columns(3).spacing([10.0, 6.0]).striped(true).show(ui, |ui| {
                ui.label("");
                ui.label(egui::RichText::new("stored").strong());
                ui.label(egui::RichText::new("calculated").strong());
                ui.end_row();
                let row = |ui: &mut egui::Ui, label: &str, a: Option<[f32; 3]>, b: Option<[f32; 3]>| {
                    ui.label(label);
                    ui.label(a.map_or("-".into(), fmt3));
                    let differs = match (a, b) {
                        (Some(a), Some(b)) => (0..3).any(|k| (a[k] - b[k]).abs() > 1.0),
                        _ => false,
                    };
                    let t = egui::RichText::new(b.map_or("-".into(), fmt3));
                    ui.label(if differs { t.color(crate::theme::warn()) } else { t });
                    ui.end_row();
                };
                row(ui, "Entities min", smin, calc.map(|c| c.entities_min));
                row(ui, "Entities max", smax, calc.map(|c| c.entities_max));
                row(ui, "Streaming min", tmin, calc.map(|c| c.streaming_min));
                row(ui, "Streaming max", tmax, calc.map(|c| c.streaming_max));
            });
            if let (Some(lo), Some(hi)) = (smin, smax) {
                let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                ui.weak(format!("The entities span {:.0} x {:.0} x {:.0} m.", size[0], size[1], size[2]));
                if lo[0] > hi[0] || lo[1] > hi[1] || lo[2] > hi[2] {
                    ui.label(egui::RichText::new("The stored extents are invalid: a minimum is above its maximum.").color(crate::theme::warn()));
                } else if size[0].max(size[1]) > 3000.0 {
                    ui.label(egui::RichText::new("Unusually large: the game streams a map in by its extents.").color(crate::theme::warn()));
                } else if count > 0 && lo == [0.0; 3] && hi == [0.0; 3] {
                    ui.label(egui::RichText::new("The extents are empty although the map has entities.").color(crate::theme::warn()));
                }
            }
            if calc.is_none() {
                ui.weak("Nothing in the map to work the extents from.");
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Calculate extents").on_hover_text("Make the extents big enough for every entity (never smaller than they are)").clicked() {
                    calc_ext = Some(ExtentsMode::GrowOnly);
                }
                if ui.button("Recalculate from scratch").on_hover_text("Work them out again from every entity, rotation, scale and LOD distance included").clicked() {
                    calc_ext = Some(ExtentsMode::Recalculate);
                }
            });
            ui.checkbox(&mut self.show_extents, "Show in the view").on_hover_text("Green: entity extents. Orange: streaming extents. Bright: stored in the file. Faint: calculated.");
        });
        egui::CollapsingHeader::new(small_heading("LOD hierarchy")).default_open(true).show(ui, |ui| {
            let errors = lod_issues.iter().filter(|i| i.1).count();
            ui.label(format!("{roots} root{} among {count} entities", if roots == 1 { "" } else { "s" }));
            let per_level: Vec<String> = [0usize, 1, 2, 3, 4, 6, 5].iter().filter(|l| levels[**l] > 0).map(|l| format!("{} {}", lod_name(*l as u8), levels[*l])).collect();
            if !per_level.is_empty() {
                ui.weak(per_level.join("  ·  "));
            }
            if errors == 0 {
                ui.label(egui::RichText::new("Every LOD relation is valid.").color(crate::theme::ok()));
            } else {
                ui.label(egui::RichText::new(format!("{errors} LOD relation{} not valid", if errors == 1 { " is" } else { "s are" })).color(crate::theme::warn()));
            }
            for (entity, is_error, msg) in lod_issues.iter().filter(|i| i.1).take(20) {
                let _ = is_error;
                ui.horizontal_wrapped(|ui| {
                    if ui.link(format!("entity {entity}")).clicked() {
                        go_lod = Some(*entity);
                    }
                    ui.label(egui::RichText::new(msg).small());
                });
            }
            if errors > 20 {
                ui.weak(format!("and {} more", errors - 20));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Rebuild").on_hover_text("Remove every LOD parent that is not a valid relation (past the end, itself, not of a higher level, part of a loop) so that entity is a root, and raise every child count that is too low. A parent in the parent map is left alone.").clicked() {
                    rebuild = Some(CountMode::RaiseOnly);
                }
                if ui.button("Rebuild, exact counts...").on_hover_text("The same, and every child count is set to the children of this map. Children in other maps are no longer counted: use it only when there are none.").clicked() {
                    let sure = rfd::MessageDialog::new().set_title("Exact child counts").set_description("Set every entity's child count to the number of its LOD children in this map? Children that live in other maps stop being counted, so only do this when no other map has LOD children of these entities.").set_buttons(rfd::MessageButtons::YesNo).show();
                    if sure == rfd::MessageDialogResult::Yes {
                        rebuild = Some(CountMode::Exact);
                    }
                }
            });
        });
        self.items_section(ui, slot);
        self.lights_section(ui, slot);
        egui::CollapsingHeader::new(small_heading("Checks")).default_open(true).show(ui, |ui| {
            if ui.button("Validate").on_hover_text("Look for broken references, LOD relations, duplicate guids, wrong flags and extents").clicked() {
                validate = true;
            }
            if self.validation.0 == Some(slot) {
                let issues = self.validation.1.clone();
                if self.validation.2 != self.proj_rev {
                    ui.weak("The map changed since this check: Validate again.");
                } else if issues.is_empty() {
                    ui.label(egui::RichText::new("No problems found.").color(crate::theme::ok()));
                }
                let mut go: Option<usize> = None;
                for i in issues.iter().take(60) {
                    let (icon, col) = if i.sev == Sev::Error { ("Error", crate::theme::warn()) } else { ("Warning", crate::theme::dim()) };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(icon).color(col));
                        match i.entity {
                            Some(e) => {
                                if ui.link(format!("entity {e}")).clicked() {
                                    go = Some(e);
                                }
                                ui.label(&i.message);
                            }
                            None => {
                                ui.label(&i.message);
                            }
                        }
                    });
                }
                if issues.len() > 60 {
                    ui.weak(format!("and {} more", issues.len() - 60));
                }
                if let Some(e) = go {
                    self.select(projmod::id(slot, e), true);
                }
            }
        });
        // ---- apply
        if let Some((n, text)) = set_field {
            self.doc_op(slot, |d| d.set_map_field(n, &text));
            self.map_bufs.clear();
            self.end_groups();
        }
        if let Some((field, bit, on)) = flag_toggle {
            self.doc_op(slot, |d| d.set_map_flag(field, bit, on));
            self.end_groups();
        }
        if let Some(mode) = rebuild {
            let r = self.doc_op(slot, |d| d.rebuild_hierarchy(mode));
            if let Some(r) = r {
                self.project_msg = if r.detached.is_empty() && r.counts_changed.is_empty() {
                    "The hierarchy already agrees with the rules.".into()
                } else {
                    format!("Removed {} invalid LOD parent{}, fixed {} child count{}.", r.detached.len(), if r.detached.len() == 1 { "" } else { "s" }, r.counts_changed.len(), if r.counts_changed.len() == 1 { "" } else { "s" })
                };
                self.end_groups();
            }
        }
        if let Some(e) = go_lod {
            self.select(projmod::id(slot, e), true);
        }
        if calc_flags {
            self.calculate_flags();
        }
        if let Some(mode) = calc_ext {
            self.calculate_extents(mode);
        }
        if validate {
            self.validate_active();
        }
    }

    /// Use the model, texture and archetype files under `dir`: the loader finds models by file name, the archetypes give sizes.
    pub(super) fn load_model_folder(&mut self, dir: &std::path::Path) {
        let mut files = vec![];
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    files.push(p);
                }
            }
        }
        let mut archetypes = 0;
        for f in files.iter().filter(|f| f.extension().map_or(false, |e| e.eq_ignore_ascii_case("ytyp"))) {
            if let Ok(list) = std::fs::read(f).map_err(anyhow::Error::from).and_then(|b| rage_toolkit_core::worldload::archetypes_of_ytyp(&b)) {
                archetypes += list.len();
                rage_toolkit_core::worldload::add_custom_archetypes(list);
            }
        }
        for f in &files {
            if let Some(stem) = f.file_stem().map(|s| s.to_string_lossy().to_string()) {
                if f.extension().map_or(false, |e| ["ydr", "yft", "ydd"].iter().any(|x| e.eq_ignore_ascii_case(x))) {
                    self.extra_names.insert(rage_toolkit_core::rage_joaat(&stem.to_lowercase()), stem);
                }
            }
        }
        let n = self.loader.as_mut().map_or(0, |l| l.add_loose(files));
        self.bump();
        self.project_msg = format!("{n} model and texture files and {archetypes} archetypes from {} are in use.", dir.display());
    }

    /// The extensions of a project entity (doors, particle effects, lights, spawn points, ladders, ...): add, remove, edit every member.
    fn extensions_section(&mut self, ui: &mut egui::Ui, id: u32) {
        use rage_toolkit_core::ymap_items::ItemList;
        let Some((slot, index)) = projmod::split(id) else { return };
        let list = ItemList::Extensions(index);
        let Some(m) = self.proj.maps.get(slot) else { return };
        let n = m.doc.item_count(list);
        let kinds = rage_toolkit_core::ymap_doc::YmapDocument::item_types(list);
        let mut act: Option<u8> = None;
        egui::CollapsingHeader::new(small_heading(&format!("Extensions ({n})"))).id_salt("extensions").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                let shown = kinds.get(self.ext_kind).map_or("-".to_string(), |k| k.0.trim_start_matches("CExtensionDef").to_string());
                egui::ComboBox::from_id_salt("ext_kind").selected_text(shown).show_ui(ui, |ui| {
                    for (i, k) in kinds.iter().enumerate() {
                        ui.selectable_value(&mut self.ext_kind, i, k.0.trim_start_matches("CExtensionDef"));
                    }
                });
                if ui.button("Add").clicked() {
                    act = Some(0);
                }
                if ui.add_enabled(self.ext_sel.is_some(), egui::Button::new(danger_text("Remove"))).clicked() {
                    act = Some(1);
                }
            });
            for i in 0..n {
                let ty = self.proj.maps[slot].doc.item_type(list, i);
                if ui.selectable_label(self.ext_sel == Some(i), format!("{i}  {}", ty.trim_start_matches("CExtensionDef"))).clicked() {
                    self.ext_sel = Some(i);
                }
            }
            let mut set: Option<(String, String)> = None;
            if let Some(i) = self.ext_sel.filter(|i| *i < n) {
                let rows = self.proj.maps[slot].doc.item_fields(list, i);
                egui::Grid::new("ext_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for r in &rows {
                        ui.label(&r.name);
                        let buf = self.field_bufs.entry(format!("ext{id}:{i}:{}", r.name)).or_insert_with(|| r.text.clone());
                        let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                        if resp.lost_focus() && *buf != r.text {
                            set = Some((r.name.clone(), buf.clone()));
                        } else if !resp.has_focus() && *buf != r.text {
                            *buf = r.text.clone();
                        }
                        ui.end_row();
                    }
                });
            }
            if let (Some((name, text)), Some(i)) = (set, self.ext_sel) {
                self.doc_op(slot, |d| d.set_item_field(list, i, &name, &text));
                self.field_bufs.retain(|k, _| !k.starts_with("ext"));
                self.end_groups();
            }
        });
        match act {
            Some(0) => {
                if let Some(k) = kinds.get(self.ext_kind) {
                    let hash = k.1;
                    self.ext_sel = self.doc_op(slot, |d| d.add_item(list, hash, None));
                }
            }
            Some(1) => {
                if let Some(i) = self.ext_sel.take() {
                    self.doc_op(slot, |d| d.remove_item(list, i));
                }
            }
            _ => {}
        }
    }

    /// LOD lights and distant lights of the active map: every light's values as text, duplicate, remove.
    fn lights_section(&mut self, ui: &mut egui::Ui, slot: usize) {
        const GROUPS: [(&str, &str); 2] = [("LOD lights", "LODLightsSOA"), ("Distant lights", "DistantLODLightsSOA")];
        egui::CollapsingHeader::new(small_heading("Lights")).default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (k, (title, group)) in GROUPS.iter().enumerate() {
                    let n = self.proj.maps[slot].doc.light_count(group);
                    if ui.selectable_label(self.light_sel.0 == k, format!("{title} ({n})")).clicked() {
                        self.light_sel = (k, None);
                    }
                }
            });
            let group = GROUPS[self.light_sel.0].1;
            let n = self.proj.maps[slot].doc.light_count(group);
            let mut act: Option<u8> = None;
            ui.horizontal(|ui| {
                if ui.add_enabled(self.light_sel.1.is_some(), egui::Button::new("Duplicate")).clicked() {
                    act = Some(0);
                }
                if ui.add_enabled(self.light_sel.1.is_some(), egui::Button::new(danger_text("Remove"))).clicked() {
                    act = Some(1);
                }
            });
            egui::ScrollArea::vertical().id_salt("lights_list").max_height(120.0).show_rows(ui, 22.0, n, |ui, range| {
                for i in range {
                    let first = self.proj.maps[slot].doc.light_values(group, i).into_iter().next().map_or(String::new(), |v| v.1);
                    if ui.selectable_label(self.light_sel.1 == Some(i), format!("{i}  {first}")).clicked() {
                        self.light_sel.1 = Some(i);
                    }
                }
            });
            let mut set: Option<(String, String)> = None;
            if let Some(i) = self.light_sel.1.filter(|i| *i < n) {
                egui::Grid::new("light_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for (name, text) in self.proj.maps[slot].doc.light_values(group, i) {
                        ui.label(&name);
                        let buf = self.field_bufs.entry(format!("light{slot}:{group}:{i}:{name}")).or_insert_with(|| text.clone());
                        let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                        if resp.lost_focus() && *buf != text {
                            set = Some((name.clone(), buf.clone()));
                        } else if !resp.has_focus() && *buf != text {
                            *buf = text.clone();
                        }
                        ui.end_row();
                    }
                });
            }
            if let (Some((name, text)), Some(i)) = (set, self.light_sel.1) {
                self.doc_op(slot, |d| d.set_light_value(group, i, &name, &text));
                self.field_bufs.retain(|k, _| !k.starts_with("light"));
                self.end_groups();
            }
            match (act, self.light_sel.1) {
                (Some(0), Some(i)) => self.light_sel.1 = self.doc_op(slot, |d| d.duplicate_light(group, i)),
                (Some(1), Some(i)) => {
                    self.light_sel.1 = None;
                    self.doc_op(slot, |d| d.remove_light(group, i));
                }
                _ => {}
            }
            ui.weak("Lights are stored as parallel lists; an edit, a copy or a removal changes the same place in all of them.");
        });
    }

    /// Car generators, box occluders, occlude models and time cycle modifiers of the active map: add, duplicate, remove, edit every member.
    fn items_section(&mut self, ui: &mut egui::Ui, slot: usize) {
        use rage_toolkit_core::ymap_items::{ItemList, MAP_LISTS};
        egui::CollapsingHeader::new(small_heading("Other data")).default_open(false).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (k, (title, member, _)) in MAP_LISTS.iter().enumerate() {
                    let n = self.proj.maps[slot].doc.item_count(ItemList::Map(member));
                    if ui.selectable_label(self.item_sel.0 == k, format!("{title} ({n})")).clicked() {
                        self.item_sel = (k, None);
                    }
                }
            });
            let (title, member, ty) = MAP_LISTS[self.item_sel.0];
            let list = ItemList::Map(member);
            let n = self.proj.maps[slot].doc.item_count(list);
            let mut act: Option<u8> = None;
            ui.horizontal(|ui| {
                if ui.button("Add").on_hover_text(format!("A new {ty} in front of the camera")).clicked() {
                    act = Some(0);
                }
                if ui.add_enabled(self.item_sel.1.is_some(), egui::Button::new("Duplicate")).clicked() {
                    act = Some(1);
                }
                if ui.add_enabled(self.item_sel.1.is_some(), egui::Button::new(danger_text("Remove"))).clicked() {
                    act = Some(2);
                }
            });
            egui::ScrollArea::vertical().id_salt("items_list").max_height(120.0).show(ui, |ui| {
                for i in 0..n {
                    let p = self.proj.maps[slot].doc.item_position(list, i).map_or(String::new(), |p| format!("  {:.0}, {:.0}, {:.0}", p.x, p.y, p.z));
                    if ui.selectable_label(self.item_sel.1 == Some(i), format!("{title} {i}{p}")).clicked() {
                        self.item_sel.1 = Some(i);
                    }
                }
            });
            let mut set: Option<(String, String)> = None;
            if let Some(i) = self.item_sel.1.filter(|i| *i < n) {
                let rows = self.proj.maps[slot].doc.item_fields(list, i);
                egui::Grid::new("item_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for r in &rows {
                        ui.label(&r.name);
                        let key = format!("item{slot}:{}:{i}:{}", self.item_sel.0, r.name);
                        let buf = self.field_bufs.entry(key).or_insert_with(|| r.text.clone());
                        let resp = ui.add(egui::TextEdit::singleline(buf).desired_width(150.0));
                        if resp.lost_focus() && *buf != r.text {
                            set = Some((r.name.clone(), buf.clone()));
                        } else if !resp.has_focus() && *buf != r.text {
                            *buf = r.text.clone();
                        }
                        ui.end_row();
                    }
                });
            }
            if let (Some((name, text)), Some(i)) = (set, self.item_sel.1) {
                self.doc_op(slot, |d| d.set_item_field(list, i, &name, &text));
                self.field_bufs.retain(|k, _| !k.starts_with("item"));
                self.end_groups();
            }
            match act {
                Some(0) => {
                    let at = self.cam.pos + self.cam.forward() * 25.0;
                    let hash = rage_toolkit_core::rage_joaat(ty);
                    if let Some(i) = self.doc_op(slot, |d| d.add_item(list, hash, Some(rage_toolkit_core::ymap_doc::Vec3::new(at.x, at.y, at.z)))) {
                        self.item_sel.1 = Some(i);
                    }
                }
                Some(1) => {
                    if let Some(i) = self.item_sel.1 {
                        self.item_sel.1 = self.doc_op(slot, |d| d.duplicate_item(list, i));
                    }
                }
                Some(2) => {
                    if let Some(i) = self.item_sel.1.take() {
                        self.doc_op(slot, |d| d.remove_item(list, i));
                    }
                }
                _ => {}
            }
            if member.starts_with("instancedData") {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.grass_radius).range(0.5..=500.0).suffix(" m")).on_hover_text("Radius");
                    if ui.button("Delete grass").on_hover_text("Delete the grass instances of this map within the radius of the selected entity (or of the point 25 m in front of the camera). One undo step.").clicked() {
                        let c = self.sel.first().and_then(|&i| self.placement_of(i)).map_or(self.cam.pos + self.cam.forward() * 25.0, |p| Vec3::from(p.pos));
                        let r = self.grass_radius;
                        if let Some(n) = self.doc_op(slot, |d| d.delete_grass_in_sphere(rage_toolkit_core::ymap_doc::Vec3::new(c.x, c.y, c.z), r)) {
                            self.project_msg = format!("Deleted {n} grass instances.");
                        }
                    }
                });
            }
            ui.weak("Members are listed as the game's table has them; a change is one undo step and a bad value is refused.");
        });
    }

    /// Save one map: its flags and extents are worked out first.
    fn save_map(&mut self, slot: usize) -> anyhow::Result<()> {
        let data = self.data();
        match &data {
            Some(d) => self.proj.save_one(slot, true, &Self::arch_bounds(d)),
            None => self.proj.save_one(slot, false, &|_| None),
        }
    }

    /// Work out the extents (from scratch) and the flags of every map of the project.
    pub(super) fn recalculate_all(&mut self) {
        let Some(d) = self.data() else { return };
        let mut changed = 0;
        for slot in 0..self.proj.maps.len() {
            let a = self.doc_op(slot, |doc| doc.update_extents(&Self::arch_bounds(&d), ExtentsMode::Recalculate)).unwrap_or(false);
            let b = self.doc_op(slot, |doc| doc.update_flags()).unwrap_or(false);
            changed += (a || b) as usize;
        }
        self.end_groups();
        self.project_msg = format!("Recalculated {} map{}: {changed} changed.", self.proj.maps.len(), if self.proj.maps.len() == 1 { "" } else { "s" });
    }

    /// Work out the active map's flags from its contents (one undo step); says what happened.
    pub(super) fn calculate_flags(&mut self) {
        let slot = self.proj.active;
        let changed = self.doc_op(slot, |d| d.update_flags());
        self.project_msg = match changed {
            Some(true) => "The flags were worked out from the contents.".into(),
            Some(false) => "The flags already match the contents.".into(),
            None => return,
        };
        self.end_groups();
    }

    /// Work out the active map's extents (one undo step); says what happened.
    pub(super) fn calculate_extents(&mut self, mode: ExtentsMode) {
        let (Some(d), slot) = (self.data(), self.proj.active) else { return };
        let changed = self.doc_op(slot, |doc| doc.update_extents(&Self::arch_bounds(&d), mode));
        self.project_msg = match changed {
            Some(true) => "The extents were calculated.".into(),
            Some(false) => "The extents already fit the entities.".into(),
            None => return,
        };
        self.end_groups();
    }

    /// Check the active map; the findings are listed in the Map panel.
    pub(super) fn validate_active(&mut self) {
        let (Some(d), slot) = (self.data(), self.proj.active) else { return };
        let Some(m) = self.proj.maps.get(slot) else { return };
        let bounds = Self::arch_bounds(&d);
        let known = |h: u32| d.arch_of(&h).is_some() || self.extra_names.contains_key(&h);
        let mut issues: Vec<MapIssue> = m.doc.validate(Some(&known), &bounds);
        // the parent map: in the project, or one of the game's (read for this check)
        let parent = m.doc.parent_name();
        if !parent.is_empty() {
            let want = parent.to_lowercase();
            let in_project = self.proj.maps.iter().enumerate().find(|(i, p)| *i != slot && (p.name.to_lowercase() == want || p.doc.name().to_lowercase() == want)).map(|(_, p)| &p.doc);
            let from_game: Option<YmapDocument> = match (in_project, self.game.as_ref()) {
                (None, Some(game)) => d.maps.iter().find(|p| leaf(p).trim_end_matches(".ymap").eq_ignore_ascii_case(&parent)).and_then(|p| game.read(p).ok()).and_then(|b| YmapDocument::open(&b, None).ok()),
                _ => None,
            };
            match in_project.or(from_game.as_ref()) {
                Some(pd) => {
                    for i in m.doc.check_against_parent_map(pd) {
                        issues.push(MapIssue { sev: if i.is_error() { Sev::Error } else { Sev::Warning }, message: format!("against the parent map {parent}: {}", i.message), entity: Some(i.entity) });
                    }
                }
                None => issues.push(MapIssue { sev: Sev::Warning, message: format!("the parent map {parent} is not in the project or the game, so the LOD parents that live in it cannot be checked"), entity: None }),
            }
        }
        self.project_msg = if issues.is_empty() { "No problems found.".into() } else { format!("{} thing{} to look at.", issues.len(), if issues.len() == 1 { "" } else { "s" }) };
        self.validation = (Some(slot), issues, self.proj_rev);
    }

    // ---- the toolbar over the view ---------------------------------------------------------------------------------------------

    pub(super) fn toolbar(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let mut new_mode = self.mode;
        let mut new_tool = self.tool;
        let selected_any = !self.sel.is_empty();
        let width = (rect.width() - 24.0).max(240.0);
        egui::Area::new(egui::Id::NULL.with("world_toolbar")).order(egui::Order::Foreground).fixed_pos(rect.left_top() + egui::vec2(12.0, 12.0)).show(ctx, |ui| {
            crate::theme::panel_frame().corner_radius(8.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                // the toolbar wraps inside the view instead of running under the side panels
                ui.set_max_width(width - 16.0);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.spacing_mut().button_padding = egui::vec2(9.0, 5.0);
                ui.horizontal_wrapped(|ui| {
                    for (m, label, tip) in [(ViewMode::Camera, "Camera", "Fly around. Hovering and clicking do nothing, so nothing gets in the way.  (Tab)"), (ViewMode::Select, "Select", "Hover to see what is under the mouse, click to select it, drag a box to select several.  (Tab)")] {
                        if ui.selectable_label(self.mode == m, egui::RichText::new(label).size(14.0)).on_hover_text(tip).clicked() {
                            new_mode = m;
                        }
                    }
                    if self.mode == ViewMode::Select {
                        ui.separator();
                        for (t, label, tip) in [(Tool::Move, "Move", "Alt+W"), (Tool::Rotate, "Rotate", "Alt+E"), (Tool::Scale, "Scale", "Alt+R")] {
                            if ui.add_enabled(selected_any, egui::Button::selectable(self.tool == t, label)).on_hover_text(tip).on_disabled_hover_text("Select something first").clicked() {
                                new_tool = t;
                            }
                        }
                        ui.separator();
                        for (s, label, tip) in [(Space::World, "World", "The handles follow the world's axes"), (Space::Local, "Local", "The handles follow the entity's own axes (one entity)")] {
                            if ui.selectable_label(self.space == s, label).on_hover_text(tip).clicked() {
                                self.space = s;
                            }
                        }
                    }
                });
                if self.mode == ViewMode::Select {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Snap");
                        for (s, label, tip) in [
                            (SnapMode::None, "Off", "Move freely"),
                            (SnapMode::Grid, "Grid", "Positions snap to the grid, turns to the angle step. Hold Alt to ignore."),
                            (SnapMode::Ground, "Ground", "Dragging across the ground keeps the entity on it. Moving straight up or down stays free, so underground is possible. Hold Alt to ignore."),
                            (SnapMode::Hybrid, "Both", "The grid for x and y, then the ground"),
                        ] {
                            if ui.selectable_label(self.snap_mode == s, label).on_hover_text(tip).clicked() {
                                self.snap_mode = s;
                            }
                        }
                        ui.add(egui::DragValue::new(&mut self.snap).speed(0.1).range(0.01..=100.0).suffix(" m")).on_hover_text("Grid size");
                        ui.add(egui::DragValue::new(&mut self.snap_angle).speed(0.5).range(1.0..=90.0).suffix("°")).on_hover_text("Angle step");
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("New YMAP").on_hover_text("An empty map in the project").clicked() {
                            self.new_map_in_project(None);
                        }
                        if ui.button("Import").on_hover_text("Bring in .ymap files").clicked() {
                            if let Some(files) = rfd::FileDialog::new().add_filter("Map file", &["ymap"]).pick_files() {
                                for f in files {
                                    self.import_ymap_file(&f);
                                }
                            }
                        }
                        if ui.button("Save").on_hover_text("Ctrl+S: the flags and extents of the changed maps are worked out first").clicked() {
                            self.save_project(true);
                        }
                        ui.separator();
                        if ui.button("Add").on_hover_text("Find a model by name and place it in front of the camera").clicked() {
                            self.open_add = true;
                            self.focus_add = true;
                        }
                        if ui.add_enabled(selected_any, egui::Button::new("Duplicate")).on_hover_text("Ctrl+D").clicked() {
                            self.duplicate_selection(Vec3::new(2.0, 0.0, 0.0));
                        }
                        if ui.add_enabled(selected_any, egui::Button::new("Delete")).on_hover_text("Delete key").clicked() {
                            self.delete_selection();
                        }
                    });
                }
            });
        });
        self.mode = new_mode;
        self.tool = new_tool;
    }
}
