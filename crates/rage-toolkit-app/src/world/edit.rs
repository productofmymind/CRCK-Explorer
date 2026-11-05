//! Editing in the World: the selection, placements, the undo history over both kinds of entity (the game's own, kept as an overlay of
//! changes, and the entities of the project's maps, which are real documents with their own history), add / duplicate / delete /
//! copy / paste, and the project on disk.
use super::{project as projmod, *};
use projmod::BASE;
use rage_toolkit_core::ymap_doc::{ArchBounds, Vec3 as RVec3};
use rage_toolkit_core::ymap_ops::Placement;
use std::collections::{BTreeMap, HashSet};

fn rv(p: [f32; 3]) -> RVec3 {
    RVec3::new(p[0], p[1], p[2])
}

pub(super) fn to_placement(e: &EntityEdit) -> Placement {
    Placement { position: rv(e.pos), rotation: e.rot, scale_xy: e.scale_xy, scale_z: e.scale_z }
}

/// what a copied entity is: one of a project map's (every member kept) or one of the game's (its model and placement)
#[derive(Clone)]
pub(super) enum ClipItem {
    Doc(rage_toolkit_core::archetype::MetaValue),
    Game { name: String, edit: EntityEdit },
}

impl World {
    pub(super) fn data(&self) -> Option<Arc<WorldData>> {
        match &self.state {
            LoadState::Ready(l) => Some(l.data.clone()),
            _ => None,
        }
    }

    /// the archetype bounds the extents need, from the loaded world
    pub(super) fn arch_bounds(data: &Arc<WorldData>) -> impl Fn(u32) -> Option<ArchBounds> + '_ {
        move |h| data.arch_of(&h).map(|a| ArchBounds { min: a.bb_min, max: a.bb_max, lod_dist: a.lod_dist })
    }

    /// the entity `id` stands for: one of the game's, or one of a project map's
    pub(super) fn ent_of(&self, id: u32) -> Option<WorldEnt> {
        if id >= BASE {
            return self.proj.world_ent(id);
        }
        match &self.state {
            LoadState::Ready(l) => l.data.ents.get(id as usize).copied(),
            _ => None,
        }
    }

    /// where the entity is now: its changed placement if it was moved, else what its file says
    pub(super) fn placement_of(&self, id: u32) -> Option<EntityEdit> {
        if id >= BASE {
            let (_, _, e) = self.proj.entity(id)?;
            return Some(EntityEdit { pos: [e.position.x, e.position.y, e.position.z], rot: e.rotation, scale_xy: e.scale_xy, scale_z: e.scale_z });
        }
        self.edits.get(&id).copied().or_else(|| self.ent_of(id).map(|e| EntityEdit::of(&e)))
    }

    pub(super) fn exists(&self, id: u32) -> bool {
        if id >= BASE {
            self.proj.entity(id).is_some()
        } else {
            !self.deleted.contains(&id) && self.ent_of(id).is_some()
        }
    }

    /// something changed that the picture or the saved project depends on
    pub(super) fn bump(&mut self) {
        self.scene_rev += 1;
        let now = std::time::Instant::now();
        self.dirty_since.get_or_insert(now);
        self.last_change = now;
    }

    /// a project map changed other than by moving entities: the entity lists are stale too
    pub(super) fn bump_docs(&mut self) {
        self.proj_rev += 1;
        self.struct_rev += 1;
        self.bump();
    }

    /// The project's entities as the world sees them, made again when a map changed.
    pub(super) fn ensure_proj_cache(&mut self) {
        if self.proj_cache.valid && self.proj_cache.rev == self.proj_rev {
            return;
        }
        let maps = (0..self.proj.maps.len()).map(|s| (0..self.proj.maps[s].doc.len()).map(|i| self.proj.world_ent(projmod::id(s, i))).collect()).collect();
        self.proj_cache = ProjCache { valid: true, rev: self.proj_rev, maps };
    }

    // ---- selection ------------------------------------------------------------------------------------------------------------

    pub(super) fn is_selected(&self, id: u32) -> bool {
        self.sel_set.contains(&id)
    }

    pub(super) fn set_selection(&mut self, ids: Vec<u32>) {
        let mut seen = HashSet::new();
        self.sel = ids.into_iter().filter(|i| seen.insert(*i)).collect();
        self.sel_set = self.sel.iter().copied().collect();
        self.selected = self.sel.last().copied();
        self.end_groups();
    }

    pub(super) fn select_only(&mut self, id: u32) {
        self.set_selection(vec![id]);
    }

    pub(super) fn toggle_select(&mut self, id: u32) {
        let mut v = self.sel.clone();
        if let Some(p) = v.iter().position(|x| *x == id) {
            v.remove(p);
        } else {
            v.push(id);
        }
        self.set_selection(v);
    }

    /// forget selected entities that no longer exist
    pub(super) fn prune_selection(&mut self) {
        let keep: Vec<u32> = self.sel.iter().copied().filter(|i| self.exists(*i)).collect();
        if keep.len() != self.sel.len() {
            self.set_selection(keep);
        }
    }

    /// what the next edit joins (a drag, a typed number) is over: the next one is a new undo step
    pub(super) fn end_groups(&mut self) {
        for m in &mut self.proj.maps {
            m.doc.end_edit_group();
        }
        self.last_edit = std::time::Instant::now() - std::time::Duration::from_secs(5);
    }

    // ---- running a change on a project map -------------------------------------------------------------------------------------

    /// Run `f` on the document in `slot`; each new step it makes in the document's history is one step of the world's. An error is shown.
    pub(super) fn doc_op<R>(&mut self, slot: usize, f: impl FnOnce(&mut YmapDocument) -> anyhow::Result<R>) -> Option<R> {
        self.doc_op_inner(slot, true, f)
    }

    /// `structural`: false for a change that only moves entities (the entity lists and the LOD tree stay as they are).
    fn doc_op_inner<R>(&mut self, slot: usize, structural: bool, f: impl FnOnce(&mut YmapDocument) -> anyhow::Result<R>) -> Option<R> {
        let m = self.proj.maps.get_mut(slot)?;
        let before = m.doc.undo_steps();
        let r = f(&mut m.doc);
        let after = m.doc.undo_steps();
        // steps the operation made before it failed are kept: they are in the document's history
        for _ in before..after {
            self.undo.push(Step::Doc(slot));
        }
        if after > before {
            self.redo.clear();
        }
        if structural {
            self.bump_docs();
        } else {
            self.proj_rev += 1;
            self.bump();
        }
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.project_msg = format!("{e:#}");
                None
            }
        }
    }

    // ---- placements -----------------------------------------------------------------------------------------------------------

    /// Set the placement of entities: a game entity's change is kept as an overlay (written out on export), a project map's entity is
    /// changed in its document. Repeated calls for the same entities within a second (a drag) are one undo step.
    pub(super) fn set_placements(&mut self, list: &[(u32, EntityEdit)]) {
        let mut game: Vec<(u32, Option<EntityEdit>, Option<EntityEdit>)> = vec![];
        let mut docs: BTreeMap<usize, Vec<(usize, Placement)>> = BTreeMap::new();
        for (id, new) in list {
            if let Some((slot, index)) = projmod::split(*id) {
                docs.entry(slot).or_default().push((index, to_placement(new)));
                continue;
            }
            let Some(orig) = self.ent_of(*id).map(|e| EntityEdit::of(&e)) else { continue };
            let before = self.edits.get(id).copied();
            let after = if *new == orig { None } else { Some(*new) };
            if before != after {
                game.push((*id, before, after));
            }
        }
        if !game.is_empty() {
            for (id, _, after) in &game {
                match after {
                    Some(a) => self.edits.insert(*id, *a),
                    None => self.edits.remove(id),
                };
            }
            let ids: Vec<u32> = game.iter().map(|g| g.0).collect();
            let join = self.last_edit.elapsed().as_secs_f32() < 1.0 && matches!(self.undo.last(), Some(Step::Edits(prev)) if prev.iter().map(|p| p.0).collect::<Vec<_>>() == ids);
            if join {
                if let Some(Step::Edits(prev)) = self.undo.last_mut() {
                    for (p, g) in prev.iter_mut().zip(&game) {
                        p.2 = g.2;
                    }
                }
                self.redo.clear();
                self.bump();
            } else {
                self.undo.push(Step::Edits(game));
                self.redo.clear();
                self.bump();
            }
            self.last_edit = std::time::Instant::now();
        }
        for (slot, items) in docs {
            // the cache of the entities is patched for just the ones moved, so a drag does not read the whole map again
            let current = self.proj_cache.valid && self.proj_cache.rev == self.proj_rev;
            // consecutive moves of the same entities are one undo step; other entities or other edits start a new one
            let key = format!("move{:?}", items.iter().map(|i| i.0).collect::<Vec<_>>());
            let done = self.doc_op_inner(slot, false, |d| d.set_placements(&items, Some(key))).is_some();
            if current && done {
                for (index, _) in &items {
                    let fresh = self.proj.world_ent(projmod::id(slot, *index));
                    if let Some(cell) = self.proj_cache.maps.get_mut(slot).and_then(|m| m.get_mut(*index)) {
                        *cell = fresh;
                    }
                }
                self.proj_cache.rev = self.proj_rev;
            }
        }
    }

    /// Move entities by `delta` (the vertical-position buttons, the arrow keys).
    pub(super) fn nudge(&mut self, ids: &[u32], delta: Vec3) {
        let list: Vec<(u32, EntityEdit)> = ids
            .iter()
            .filter_map(|&id| {
                let mut e = self.placement_of(id)?;
                e.pos = [e.pos[0] + delta.x, e.pos[1] + delta.y, e.pos[2] + delta.z];
                Some((id, e))
            })
            .collect();
        self.set_placements(&list);
        self.end_groups();
    }

    // ---- undo and redo ----------------------------------------------------------------------------------------------------------

    fn apply_step(&mut self, step: &Step, forward: bool) {
        match step {
            Step::Edits(items) => {
                for (id, before, after) in items {
                    match if forward { after } else { before } {
                        Some(e) => self.edits.insert(*id, *e),
                        None => self.edits.remove(id),
                    };
                }
                let ids: Vec<u32> = items.iter().map(|i| i.0).collect();
                self.set_selection(ids);
            }
            Step::DelGame(ids) => {
                for id in ids {
                    if forward {
                        self.deleted.insert(*id);
                    } else {
                        self.deleted.remove(id);
                    }
                }
                if !forward {
                    self.set_selection(ids.clone());
                }
            }
            Step::Doc(slot) => {
                if let Some(m) = self.proj.maps.get_mut(*slot) {
                    let r = if forward { m.doc.redo() } else { m.doc.undo() };
                    if let Err(e) = r {
                        self.project_msg = format!("{e:#}");
                    }
                }
                // the entity lists and the LOD tree are stale too (an undone add, delete or reparent changes them)
                self.bump_docs();
            }
        }
        self.prune_selection();
        self.bump();
    }

    pub(super) fn undo_step(&mut self) {
        let Some(u) = self.undo.pop() else { return };
        self.apply_step(&u, false);
        self.redo.push(u);
        self.last_edit = std::time::Instant::now() - std::time::Duration::from_secs(5);
    }

    pub(super) fn redo_step(&mut self) {
        let Some(u) = self.redo.pop() else { return };
        self.apply_step(&u, true);
        self.undo.push(u);
        self.last_edit = std::time::Instant::now() - std::time::Duration::from_secs(5);
    }

    /// Forget every undo step (the project's maps changed in a way ids cannot follow).
    pub(super) fn clear_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    // ---- adding, copying, removing ------------------------------------------------------------------------------------------

    /// Whether map `slot` can take `n` more entities (an entity id holds a 20-bit index); says so when it cannot.
    fn has_room(&mut self, slot: usize, n: usize) -> bool {
        let used = self.proj.maps.get(slot).map_or(0, |m| m.doc.len());
        if used + n > projmod::MAX_ENTITIES {
            self.project_msg = format!("A map holds at most {} entities.", projmod::MAX_ENTITIES);
            return false;
        }
        true
    }

    /// The map new entities go into; one is made when the project has none.
    pub(super) fn active_slot(&mut self) -> Option<usize> {
        if self.proj.maps.is_empty() {
            self.new_map_in_project(None)?;
        }
        Some(self.proj.active.min(self.proj.maps.len().saturating_sub(1)))
    }

    /// A new empty map in the project (made the active one). Returns its slot.
    pub(super) fn new_map_in_project(&mut self, name: Option<&str>) -> Option<usize> {
        match self.proj.new_map(name) {
            Ok(slot) => {
                self.bump_docs();
                self.project_msg = format!("Made the map {}.", self.proj.maps[slot].name);
                Some(slot)
            }
            Err(e) => {
                self.project_msg = format!("{e:#}");
                None
            }
        }
    }

    /// A new entity of `name` with this placement, in the active map; it becomes the selection. Returns its id.
    pub(super) fn add_entity(&mut self, name: &str, edit: EntityEdit) -> Option<u32> {
        let slot = self.active_slot()?;
        if !self.has_room(slot, 1) {
            return None;
        }
        let p = to_placement(&edit);
        let mlo = self.data().map_or(false, |d| d.arch_of(&rage_toolkit_core::rage_joaat(&name.to_lowercase())).map_or(false, |a| a.is_mlo));
        let index = self.doc_op(slot, |d| d.add_entity_as(name, p, mlo))?;
        self.extra_names.insert(rage_toolkit_core::rage_joaat(&name.to_lowercase()), name.to_string());
        let id = projmod::id(slot, index);
        self.select_only(id);
        Some(id)
    }

    /// Copies of the selected entities, `offset` away, in the active map (the copies become the selection). A project map's entity is copied with
    /// all its members; a game entity becomes a new entity with its model and placement. Returns the new ids.
    pub(super) fn duplicate_selection(&mut self, offset: Vec3) -> Vec<u32> {
        let ids = self.sel.clone();
        let mut out = vec![];
        // the project's entities go map by map, each map's copies in one step
        let mut by_slot: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &id in &ids {
            if let Some((slot, index)) = projmod::split(id) {
                by_slot.entry(slot).or_default().push(index);
            }
        }
        for (slot, mut idx) in by_slot {
            idx.sort_unstable();
            idx.dedup();
            if !self.has_room(slot, idx.len()) {
                continue;
            }
            let new = self.doc_op(slot, |d| {
                let items = d.copy_entities(&idx);
                d.paste_entities(&items, rv([offset.x, offset.y, offset.z]))
            });
            out.extend(new.unwrap_or_default().into_iter().map(|i| projmod::id(slot, i)));
        }
        for id in ids {
            if projmod::split(id).is_some() {
                continue;
            }
            if let Some(e) = self.ent_of(id) {
                let name = self.name(e.hash);
                if name.starts_with("hash_") {
                    self.project_msg = "That entity's model has no readable name, so it cannot be copied.".into();
                    continue;
                }
                let mut edit = self.placement_of(id).unwrap_or_else(|| EntityEdit::of(&e));
                edit.pos = [edit.pos[0] + offset.x, edit.pos[1] + offset.y, edit.pos[2] + offset.z];
                if let Some(slot) = self.active_slot() {
                    let p = to_placement(&edit);
                    if let Some(index) = self.doc_op(slot, |d| d.add_entity_at(&name, p)) {
                        out.push(projmod::id(slot, index));
                    }
                }
            }
        }
        if !out.is_empty() {
            self.set_selection(out.clone());
        }
        out
    }

    /// Take the selected entities out of the map. A game entity is hidden (and left out of its file on export); a project map's entity is
    /// removed from its document, which refuses while it is the LOD parent of others.
    pub(super) fn delete_selection(&mut self) {
        let ids = self.sel.clone();
        let mut game: Vec<u32> = vec![];
        let mut docs: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for id in ids {
            match projmod::split(id) {
                Some((slot, index)) => docs.entry(slot).or_default().push(index),
                None => game.push(id),
            }
        }
        if !game.is_empty() {
            let fresh: Vec<u32> = game.into_iter().filter(|g| self.deleted.insert(*g)).collect();
            if !fresh.is_empty() {
                self.undo.push(Step::DelGame(fresh));
                self.redo.clear();
                self.bump();
            }
        }
        // what could not go (a LOD parent whose children stay) stays selected; the rest are gone and their ids are not valid any more
        let mut keep: Vec<u32> = vec![];
        for (slot, mut idx) in docs {
            idx.sort_unstable();
            idx.dedup();
            if self.doc_op(slot, |d| d.remove_entities(&idx)).is_none() {
                keep.extend(idx.iter().map(|i| projmod::id(slot, *i)));
            }
        }
        self.set_selection(keep);
    }

    /// Remember the selected entities for [`World::paste`].
    pub(super) fn copy_selection(&mut self) {
        let mut items = vec![];
        for &id in &self.sel {
            if let Some((slot, index)) = projmod::split(id) {
                if let Some(m) = self.proj.maps.get(slot) {
                    items.extend(m.doc.copy_entities(&[index]).into_iter().map(ClipItem::Doc));
                }
            } else if let Some(e) = self.ent_of(id) {
                let name = self.name(e.hash);
                if !name.starts_with("hash_") {
                    items.push(ClipItem::Game { name, edit: self.placement_of(id).unwrap_or_else(|| EntityEdit::of(&e)) });
                }
            }
        }
        if items.is_empty() {
            self.project_msg = "Nothing to copy.".into();
        } else {
            self.project_msg = format!("Copied {} entit{}.", items.len(), if items.len() == 1 { "y" } else { "ies" });
        }
        self.clipboard = items;
    }

    /// Paste the copied entities into the active map with their middle at `at`; the pasted ones become the selection.
    pub(super) fn paste(&mut self, at: Vec3) {
        if self.clipboard.is_empty() {
            return;
        }
        let positions: Vec<Vec3> = self
            .clipboard
            .iter()
            .filter_map(|c| match c {
                ClipItem::Game { edit, .. } => Some(Vec3::from(edit.pos)),
                ClipItem::Doc(v) => v.as_struct().and_then(|s| s.field("position")).and_then(|p| p.as_vec3()).map(|p| Vec3::new(p.x, p.y, p.z)),
            })
            .collect();
        let centre = if positions.is_empty() { Vec3::ZERO } else { positions.iter().copied().sum::<Vec3>() / positions.len() as f32 };
        let offset = at - centre;
        let Some(slot) = self.active_slot() else { return };
        if !self.has_room(slot, self.clipboard.len()) {
            return;
        }
        let docs: Vec<_> = self.clipboard.iter().filter_map(|c| if let ClipItem::Doc(v) = c { Some(v.clone()) } else { None }).collect();
        let games: Vec<(String, EntityEdit)> = self.clipboard.iter().filter_map(|c| if let ClipItem::Game { name, edit } = c { Some((name.clone(), *edit)) } else { None }).collect();
        let mut out = vec![];
        if !docs.is_empty() {
            if let Some(new) = self.doc_op(slot, |d| d.paste_entities(&docs, rv([offset.x, offset.y, offset.z]))) {
                out.extend(new.into_iter().map(|i| projmod::id(slot, i)));
            }
        }
        for (name, mut edit) in games {
            edit.pos = [edit.pos[0] + offset.x, edit.pos[1] + offset.y, edit.pos[2] + offset.z];
            let p = to_placement(&edit);
            if let Some(i) = self.doc_op(slot, |d| d.add_entity_at(&name, p)) {
                out.push(projmod::id(slot, i));
            }
        }
        if !out.is_empty() {
            self.set_selection(out);
        }
    }

    /// Put a game entity back where the game has it (and bring back a deleted one).
    pub(super) fn reset_selection(&mut self) {
        let ids: Vec<u32> = self.sel.iter().copied().filter(|i| *i < BASE).collect();
        let list: Vec<(u32, EntityEdit)> = ids.iter().filter_map(|&i| self.ent_of(i).map(|e| (i, EntityEdit::of(&e)))).collect();
        self.set_placements(&list);
        self.end_groups();
    }

    /// Forget every change to the game's maps (the project's own maps stay).
    pub(super) fn reset_game_changes(&mut self) {
        self.edits.clear();
        self.deleted.clear();
        self.clear_history();
        self.prune_selection();
        self.bump();
        self.project_msg = "The changes to the game's entities were reset.".into();
    }

    // ---- maps of the project ----------------------------------------------------------------------------------------------------

    /// Generate the LOD-light maps (`<name>_lodlights`, `<name>_distantlights`) from the lights of the models the project's maps place.
    pub(super) fn generate_lod_lights(&mut self, name: &str) {
        use rage_toolkit_core::lodlights::{build_lod_light_maps, lights_of_entity};
        let (Some(data), Some(game), LoadState::Ready(l)) = (self.data(), self.game.clone(), &self.state) else { return };
        let files = l.files.clone();
        let names = l.names.clone();
        let mut cache: HashMap<u32, Vec<rage_toolkit_core::lodlights::DrawableLight>> = HashMap::new();
        let mut lights = vec![];
        for m in &self.proj.maps {
            for i in 0..m.doc.len() {
                let Some(e) = m.doc.entity(i) else { continue };
                let list = cache.entry(e.archetype_hash).or_insert_with(|| {
                    let arch = data.arch_of(&e.archetype_hash);
                    let asset = arch.and_then(|a| names.get(&a.asset_hash)).or_else(|| names.get(&e.archetype_hash)).map(|n| n.to_lowercase());
                    asset.and_then(|n| files.get(&format!("{n}.ydr"))).and_then(|p| game.read(p).ok()).and_then(|b| rage_toolkit_core::lodlights::parse_ydr_lights(&b).ok()).unwrap_or_default()
                });
                if list.is_empty() {
                    continue;
                }
                let bb = data.arch_of(&e.archetype_hash).map_or(([-1.0; 3], [1.0; 3]), |a| (a.bb_min, a.bb_max));
                lights.extend(lights_of_entity(list, [e.position.x, e.position.y, e.position.z], e.rotation, [e.scale_xy, e.scale_xy, e.scale_z], bb, 0));
            }
        }
        if lights.is_empty() {
            self.project_msg = "No model of the project's maps carries lights.".into();
            return;
        }
        let n = lights.len();
        match build_lod_light_maps(name, lights) {
            Ok((lod, dist)) => {
                for (nm, d) in [(format!("{name}_distantlights"), dist), (format!("{name}_lodlights"), lod)] {
                    if let Err(e) = self.proj.add_doc(&nm, d) {
                        self.project_msg = format!("{e:#}");
                        return;
                    }
                }
                self.bump_docs();
                self.project_msg = format!("Made {name}_lodlights and {name}_distantlights from {n} lights.");
            }
            Err(e) => self.project_msg = format!("{e:#}"),
        }
    }

    /// Write a `_manifest.ymf.xml` for the project's maps: which `.ytyp` files each map's entities and grass use.
    pub(super) fn export_manifest(&mut self) {
        use rage_toolkit_core::{manifest::{manifest_xml, ManifestMap}, ymap_items::ItemList};
        let Some(data) = self.data() else { return };
        let stem = |idx: u32| data.ytyps.get(idx as usize).map(|p| p.rsplit('/').next().unwrap_or(p).trim_end_matches(".ytyp").to_lowercase());
        let mut maps = vec![];
        for m in &self.proj.maps {
            let mut ytyps = std::collections::BTreeSet::new();
            let mut interior = false;
            for i in 0..m.doc.len() {
                if let Some(e) = m.doc.entity(i) {
                    interior |= e.is_mlo_instance;
                    if let Some(y) = data.arch_of(&e.archetype_hash).and_then(|a| stem(a.ytyp)) {
                        ytyps.insert(y);
                    }
                }
            }
            let grass = ItemList::Map("instancedData/GrassInstanceList");
            for i in 0..m.doc.item_count(grass) {
                if let Some(r) = m.doc.item_fields(grass, i).into_iter().find(|r| r.name == "archetypeName") {
                    if let Some(y) = data.arch_of(&rage_toolkit_core::archetype::schema_view::name_hash(&r.text)).and_then(|a| stem(a.ytyp)) {
                        ytyps.insert(y);
                    }
                }
            }
            maps.push(ManifestMap { name: m.doc.name(), ytyps, interior });
        }
        if maps.is_empty() {
            self.project_msg = "There are no maps in the project.".into();
            return;
        }
        let xml = manifest_xml(&maps, &[]);
        if let Some(f) = rfd::FileDialog::new().set_file_name("_manifest.ymf.xml").set_title("Save the manifest").save_file() {
            self.project_msg = match std::fs::write(&f, xml) {
                Ok(()) => format!("Wrote the manifest for {} map{} to {}.", maps.len(), if maps.len() == 1 { "" } else { "s" }, f.display()),
                Err(e) => format!("{}: {e}", f.display()),
            };
        }
    }

    /// Show the kinds of entity (HD, LOD, SLOD, interiors) map `slot` has: a map of LOD entities is no use with the LOD layer hidden.
    pub(super) fn reveal_layers_of(&mut self, slot: usize) {
        let Some(m) = self.proj.maps.get(slot) else { return };
        let (mut hd, mut lod, mut slod, mut interiors) = (false, false, false, false);
        for i in 0..m.doc.len() {
            if let Some(e) = m.doc.entity(i) {
                if e.is_mlo_instance {
                    interiors = true;
                } else {
                    match e.lod_level {
                        0 | 5 => hd = true,
                        1 => lod = true,
                        _ => slod = true,
                    }
                }
            }
        }
        self.layers.hd |= hd;
        self.layers.lod |= lod;
        self.layers.slod |= slod;
        self.layers.interiors |= interiors;
    }

    /// Show the layer an entity of this LOD level is on (after the level was set, so the entity does not vanish from the view).
    pub(super) fn reveal_level(&mut self, level: u8) {
        match level {
            0 | 5 => self.layers.hd = true,
            1 => self.layers.lod = true,
            _ => self.layers.slod = true,
        }
    }

    /// Bring a `.ymap` of the user's into the project.
    pub(super) fn import_ymap_file(&mut self, path: &std::path::Path) {
        match self.proj.import_file(path) {
            Ok(slot) => {
                self.reveal_layers_of(slot);
                self.bump_docs();
                self.project_msg = format!("Imported {} ({} entities). The original is not changed until you save to it.", self.proj.maps[slot].name, self.proj.maps[slot].doc.len());
            }
            Err(e) => self.project_msg = format!("{e:#}"),
        }
    }

    /// Import a Menyoo / Spooner placement file as a new map of the project: each object becomes a static (or dynamic) orphan-HD entity.
    pub(super) fn import_menyoo(&mut self, path: &std::path::Path) {
        let placements = match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| rage_toolkit_core::menyoo::parse(&t)) {
            Ok(p) => p,
            Err(e) => {
                self.project_msg = format!("{}: {e:#}", path.display());
                return;
            }
        };
        let objects: Vec<_> = placements.into_iter().filter(|p| p.kind == 3).collect();
        if objects.is_empty() {
            self.project_msg = "That file has no objects (placements of type 3).".into();
            return;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "menyoo".into());
        let Some(slot) = self.new_map_in_project(Some(&stem)) else { return };
        let list: Vec<_> = objects
            .iter()
            .map(|o| (o.archetype_name(), Placement { position: rv(o.position), rotation: o.stored_rotation(), scale_xy: 1.0, scale_z: 1.0 }, if o.dynamic { 0 } else { 32 }, 5u8, o.lod_distance.min(10000.0)))
            .collect();
        let n = list.len();
        if self.doc_op(slot, |d| d.add_entities_bulk(&list)).is_some() {
            self.reveal_layers_of(slot);
            self.project_msg = format!("Imported {n} objects from {}.", path.file_name().unwrap_or_default().to_string_lossy());
        }
    }

    /// Open one of the game's maps for editing as a copy in the project: the changes already made to its entities move into the copy, and the
    /// game's own entities of that map are no longer drawn (the copy is).
    pub(super) fn import_game_map(&mut self, map: u32) {
        let (Some(data), Some(game)) = (self.data(), self.game.clone()) else { return };
        let Some(path) = data.maps.get(map as usize).cloned() else { return };
        let bytes = match game.read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.project_msg = format!("{path}: {e:#}");
                return;
            }
        };
        let leaf = path.rsplit('/').next().unwrap_or(&path).trim_end_matches(".ymap").to_string();
        let slot = match self.proj.import_game(&leaf, &bytes, &path) {
            Ok(s) => s,
            Err(e) => {
                self.project_msg = format!("{e:#}");
                return;
            }
        };
        // the entities of that map in the world, in file order: global id = first + index
        let first = data.ents.iter().position(|e| e.map == map);
        let mut selected_map: Vec<u32> = vec![];
        if let Some(first) = first {
            let count = data.ents[first..].iter().take_while(|e| e.map == map).count();
            let ids: Vec<u32> = (first as u32..(first + count) as u32).collect();
            // placements changed so far
            let moved: Vec<(usize, Placement)> = ids.iter().filter_map(|g| self.edits.get(g).map(|e| (data.ents[*g as usize].index as usize, to_placement(e)))).collect();
            let gone: Vec<usize> = ids.iter().filter(|g| self.deleted.contains(g)).map(|g| data.ents[*g as usize].index as usize).collect();
            for g in &ids {
                self.edits.remove(g);
                self.deleted.remove(g);
            }
            if !moved.is_empty() || !gone.is_empty() {
                self.doc_op(slot, |d| {
                    d.set_placements(&moved, Some("import".into()))?;
                    d.remove_entities(&gone)
                });
            }
            for &s in &self.sel {
                if ids.contains(&s) {
                    selected_map.push(projmod::id(slot, data.ents[s as usize].index as usize));
                }
            }
        }
        self.clear_history();
        self.rebuild_hidden();
        self.reveal_layers_of(slot);
        self.bump_docs();
        if !selected_map.is_empty() {
            self.set_selection(selected_map);
        } else {
            self.prune_selection();
        }
        self.project_msg = format!("{leaf} is open for editing. Your changes to it so far are in the copy.");
    }

    /// Which of the game's maps have an editable copy in the project (their own entities are not drawn).
    pub(super) fn rebuild_hidden(&mut self) {
        self.hidden_maps.clear();
        let Some(data) = self.data() else { return };
        // maps of DLC packs later than the chosen level are not drawn, nor the game's archives that a pack up to it replaces with its own (the heists'
        // `hei_` maps take the place of the game's, the lowriders' `lr_` ones of those)
        if let LoadState::Ready(l) = &self.state {
            for (i, hide) in l.dlc_maps.hidden(self.dlc_level).into_iter().enumerate() {
                if hide {
                    self.hidden_maps.insert(i as u32);
                }
            }
        }
        let by_path: HashMap<&str, u32> = data.maps.iter().enumerate().map(|(i, m)| (m.as_str(), i as u32)).collect();
        for m in &self.proj.maps {
            if m.kind == MapKind::Game {
                if let Some(idx) = m.source.as_deref().and_then(|s| by_path.get(s)) {
                    self.hidden_maps.insert(*idx);
                }
            }
        }
    }

    /// Take a map out of the project (its working copy is deleted; the original it came from is not touched).
    pub(super) fn remove_project_map(&mut self, slot: usize) {
        self.proj.remove(slot);
        // entity ids of the later maps changed
        self.clear_history();
        self.set_selection(vec![]);
        self.rebuild_hidden();
        self.bump_docs();
    }

    // ---- the project on disk ---------------------------------------------------------------------------------------------------

    pub(super) fn project_file(&self) -> std::path::PathBuf {
        self.proj.dir.join("project.crckproj")
    }

    /// where projects live unless the user picks another folder
    pub(super) fn default_project_dir() -> std::path::PathBuf {
        rage_toolkit_core::game::cache_dir().parent().map(|p| p.join("projects")).unwrap_or_default().join("default")
    }

    fn last_project_file() -> std::path::PathBuf {
        rage_toolkit_core::game::cache_dir().parent().map(|p| p.join("projects")).unwrap_or_default().join("last_project.txt")
    }

    pub(super) fn remember_project_dir(&self) {
        // an automated screenshot run uses a scratch project and leaves the user's choice alone
        if self.automation {
            return;
        }
        let _ = std::fs::create_dir_all(Self::last_project_file().parent().unwrap_or(std::path::Path::new(".")));
        let _ = std::fs::write(Self::last_project_file(), self.proj.dir.display().to_string());
    }

    /// the project folder to open at start: the one used last, else the default
    pub(super) fn last_project_dir() -> std::path::PathBuf {
        std::fs::read_to_string(Self::last_project_file()).ok().map(|s| std::path::PathBuf::from(s.trim())).filter(|p| p.join("project.crckproj").exists() || p == &Self::default_project_dir()).unwrap_or_else(Self::default_project_dir)
    }

    /// the project as the file lists it
    pub(super) fn to_project(&self) -> Project {
        let mut p = Project::default();
        p.new_map = self.new_map.clone();
        p.maps = self.proj.refs();
        p.active = self.proj.active_map().map(|m| m.name.clone());
        if let LoadState::Ready(l) = &self.state {
            for (&i, e) in &self.edits {
                if let Some(ent) = l.data.ents.get(i as usize) {
                    p.edits.insert(EntityKey::of(&l.data, ent), *e);
                }
            }
            for &i in &self.deleted {
                if let Some(ent) = l.data.ents.get(i as usize) {
                    p.deleted.insert(EntityKey::of(&l.data, ent));
                }
            }
        }
        p
    }

    /// Write the project file and the working copies of the maps that changed. `update_all` first works out the flags and extents of the changed
    /// maps (what Save does; the automatic save leaves the maps exactly as edited).
    pub(super) fn save_project(&mut self, update_all: bool) {
        let data = self.data();
        let problems = match &data {
            Some(d) => self.proj.save_all(update_all, &Self::arch_bounds(d)),
            None => self.proj.save_all(false, &|_| None),
        };
        let p = self.to_project();
        let mut ok = problems.is_empty();
        if let Err(e) = p.save(&self.project_file()) {
            ok = false;
            self.project_msg = format!("Could not save the project: {e:#}");
        } else if !problems.is_empty() {
            self.project_msg = format!("Could not save: {}", problems.join("; "));
        } else if update_all {
            self.project_msg = "Saved. The flags and extents of the changed maps were calculated first.".into();
        }
        self.remember_project_dir();
        // a failed save leaves the changes pending, so the autosave tries again
        if ok {
            self.dirty_since = None;
        } else {
            self.dirty_since = Some(std::time::Instant::now());
        }
    }

    /// Read the project of `dir` once the world is loaded: its maps from their working copies, and the changes to game entities that still
    /// match the game's files.
    pub(super) fn load_project_from(&mut self, dir: std::path::PathBuf) {
        self.project_loaded = true;
        let Some(data) = self.data() else { return };
        self.edits.clear();
        self.deleted.clear();
        self.clear_history();
        self.set_selection(vec![]);
        let loaded = Project::load(&dir.join("project.crckproj")).or_else(|| {
            // the file older versions kept the changes in
            (dir == Self::default_project_dir()).then(|| dir.parent().map(|p| p.join("world.crckproj"))).flatten().and_then(|f| Project::load(&f))
        });
        let Some(p) = loaded else {
            self.proj = ProjectMaps::new(dir);
            self.rebuild_hidden();
            self.bump_docs();
            return;
        };
        let (proj, warnings) = ProjectMaps::load(dir, &p.maps, p.active.as_deref());
        self.proj = proj;
        self.new_map = p.new_map.clone();
        let mut start: HashMap<u32, u32> = HashMap::new();
        for (i, e) in data.ents.iter().enumerate() {
            start.entry(e.map).or_insert(i as u32);
        }
        let map_of: HashMap<&str, u32> = data.maps.iter().enumerate().map(|(i, m)| (m.as_str(), i as u32)).collect();
        let find = |k: &EntityKey| -> Option<u32> {
            let g = map_of.get(k.map.as_str()).and_then(|m| start.get(m)).map(|s| s + k.index)?;
            let ent = data.ents.get(g as usize)?;
            (ent.map == map_of[k.map.as_str()] && ent.index == k.index && ent.hash == k.hash && ent.guid == k.guid).then_some(g)
        };
        let (mut kept, mut dropped) = (0, 0);
        for (k, e) in &p.edits {
            match find(k) {
                Some(g) => {
                    self.edits.insert(g, *e);
                    kept += 1;
                }
                None => dropped += 1,
            }
        }
        for k in &p.deleted {
            match find(k) {
                Some(g) => {
                    self.deleted.insert(g);
                    kept += 1;
                }
                None => dropped += 1,
            }
        }
        // entities a version 2 project added: they go into a map of their own
        if !p.added.is_empty() {
            if let Some(slot) = self.proj.new_map(Some(&p.new_map)).ok() {
                for n in &p.added {
                    let pl = to_placement(&n.edit);
                    let _ = self.proj.maps[slot].doc.add_entity_at(&n.archetype, pl);
                }
                kept += p.added.len();
            }
        }
        self.clear_history();
        self.rebuild_hidden();
        self.bump_docs();
        let mut msg = vec![];
        if !self.proj.maps.is_empty() {
            msg.push(format!("{} map{}", self.proj.maps.len(), if self.proj.maps.len() == 1 { "" } else { "s" }));
        }
        if kept > 0 {
            msg.push(format!("{kept} change{}", if kept == 1 { "" } else { "s" }));
        }
        if !msg.is_empty() {
            self.project_msg = format!("Reopened the project: {}.", msg.join(", "));
        }
        if dropped > 0 {
            self.project_msg.push_str(&format!(" {dropped} change{} no longer match the game's files and were left out.", if dropped == 1 { "" } else { "s" }));
        }
        if !warnings.is_empty() {
            self.project_msg.push_str(&format!(" {}", warnings.join(" ")));
        }
    }

    /// Start a project in `dir` (a folder of the user's choice); what is open now is saved first.
    pub(super) fn switch_project(&mut self, dir: std::path::PathBuf, fresh: bool) {
        self.save_project(false);
        // a folder that already holds a project is opened, never overwritten with an empty one
        let fresh = fresh && !dir.join("project.crckproj").exists();
        if fresh {
            self.edits.clear();
            self.deleted.clear();
            self.clear_history();
            self.set_selection(vec![]);
            self.proj = ProjectMaps::new(dir);
            self.rebuild_hidden();
            self.bump_docs();
            self.save_project(false);
            self.project_msg = "A new project.".into();
        } else {
            self.load_project_from(dir);
            self.remember_project_dir();
        }
    }

    /// Write the project when it changed and nothing changed for a second (or when the first unsaved change is ten seconds old, so a long
    /// stretch of editing is never entirely unsaved). Never in the middle of a drag.
    pub(super) fn autosave(&mut self, ctx: &egui::Context) {
        let Some(first) = self.dirty_since else { return };
        if self.drag.is_some() || (self.last_change.elapsed().as_secs_f32() < 1.0 && first.elapsed().as_secs_f32() < 10.0) {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            return;
        }
        self.save_project(false);
    }

    /// Write the project's maps and the changed game maps to a folder of the user's choice: the flags and extents are worked out first
    /// (grown only, unless asked to recalculate), and the game itself is never changed.
    pub(super) fn export(&mut self) {
        let (Some(data), Some(game)) = (self.data(), self.game.clone()) else { return };
        self.save_project(true);
        let p = self.to_project();
        if p.is_empty() {
            self.project_msg = "There is nothing to export.".into();
            return;
        }
        let Some(dir) = rfd::FileDialog::new().set_title("Export the map files to...").pick_folder() else { return };
        let mode = if self.recalc_extents { ExtentsMode::Recalculate } else { ExtentsMode::GrowOnly };
        self.project_msg = match worldproject::export_with_maps(&game, &data, &p, Some(&self.proj.dir.clone()), &dir, mode) {
            Ok(done) => format!(
                "Wrote {} map file{} to {} (the game itself was not changed): {}",
                done.len(),
                if done.len() == 1 { "" } else { "s" },
                dir.display(),
                done.iter().map(|d| format!("{} ({} moved, {} removed)", d.file.file_name().unwrap_or_default().to_string_lossy(), d.entities_changed, d.entities_deleted)).collect::<Vec<_>>().join("; ")
            ),
            Err(e) => format!("Export failed, nothing was written for the failing map: {e:#}"),
        };
    }
}
