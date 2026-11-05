//! Runs only on a machine with GTA V installed. Moves entities of a real map in a project, exports, and reads the result back.
use rage_toolkit_core::{
    game::Game,
    index::FileIndex,
    job::Job,
    worldload::WorldData,
    worldproject::{self, EntityEdit, EntityKey, Project},
    ymap_doc::{ExtentsMode, YmapDocument},
};

#[test]
fn a_moved_entity_of_a_real_map_is_exported_with_its_extents() {
    let Ok(game) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let ix = FileIndex::load_or_build(&game, &Job::new(), false).unwrap();
    let w = WorldData::load(&game, &ix, &Job::new()).unwrap();
    let map = w.maps.iter().position(|m| m.ends_with("/cs1_02_long_0.ymap")).expect("the test map");
    let ents: Vec<_> = w.ents.iter().filter(|e| e.map as usize == map && !e.is_mlo).collect();
    let (a, b) = (ents[5], ents[9]);
    let mut p = Project::default();
    let mut moved = worldproject::EntityEdit::of(a);
    moved.pos[0] += 10.0;
    moved.pos[2] += 500.0; // far above the map: the extents must grow
    p.edits.insert(EntityKey::of(&w, a), moved);
    p.edits.insert(EntityKey::of(&w, b), EntityEdit { scale_xy: 2.0, ..EntityEdit::of(b) });
    // one entity taken out (one that is nobody's LOD parent), one added to a new map
    let gone = ents.iter().find(|e| e.index > 12 && e.num_children == 0).copied().unwrap();
    p.deleted.insert(EntityKey::of(&w, gone));
    p.new_map = "crck_test_additions".into();
    p.added.push(worldproject::NewEntity { archetype: "prop_bench_01a".into(), edit: EntityEdit { pos: [100.0, 200.0, 30.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 } });
    let out = std::env::temp_dir().join(format!("crck_world_export_{}", std::process::id()));
    let done = worldproject::export(&game, &w, &p, &out, ExtentsMode::GrowOnly).unwrap();
    assert_eq!(done.len(), 2, "the edited map and the new one");
    let edited = done.iter().find(|d| d.file.ends_with("cs1_02_long_0.ymap")).unwrap();
    assert_eq!((edited.entities_changed, edited.entities_deleted, edited.extents_changed), (2, 1, true));
    let created = YmapDocument::open_file(&out.join("crck_test_additions.ymap")).unwrap();
    assert_eq!((created.len(), created.entity(0).unwrap().position.z), (1, 30.0));

    let orig = YmapDocument::open(&game.read(&w.maps[map]).unwrap(), None).unwrap();
    let back = YmapDocument::open_file(&out.join("cs1_02_long_0.ymap")).unwrap();
    assert_eq!(back.len(), orig.len() - 1, "one entity was deleted");
    for i in 0..a.index as usize + 1 {
        let (o, n) = (orig.entity(i).unwrap(), back.entity(i).unwrap());
        if i == a.index as usize {
            assert_eq!((n.position.x, n.position.z), (o.position.x + 10.0, o.position.z + 500.0));
        } else {
            // removing an entity moves every LOD parent index above it down by one, except for an entity whose parent is in the parent
            // map (flag 8): that index points into another file and is not this file's to change
            let mut o = o;
            if o.parent_index > gone.index as i32 && o.flags & 8 == 0 {
                o.parent_index -= 1;
            }
            assert_eq!(n, o, "entity {i} was not edited");
        }
    }
    let (ob, nb) = (orig.entity(b.index as usize).unwrap(), back.entity(b.index as usize).unwrap());
    assert_eq!((nb.scale_xy, nb.position), (2.0, ob.position));
    let top = back.to_xml();
    assert!(top.contains("entitiesExtentsMax"), "{top}");
    let max_z = |d: &YmapDocument| d.computed_extents(&|_| None).map(|e| e.1[2]).unwrap();
    assert!(max_z(&back) > max_z(&orig) + 400.0);
    assert!(out.join("CRCK-changes.txt").exists());
    let _ = std::fs::remove_dir_all(&out);
}
