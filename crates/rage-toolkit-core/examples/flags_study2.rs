//! The flag rules against the game's own maps, using the whole map tree (so occluders, physics dictionaries, lights and
//! grass count too). Prints, per flag bit, how often the stored bit and the rule agree, and what the disagreeing maps contain.
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ymap_doc::YmapDocument};
use rage_formats::MetaValue;
use std::collections::BTreeMap;

fn count(root: &MetaValue, path: &[&str]) -> usize {
    let mut v = root;
    for p in path {
        match v.as_struct().and_then(|s| s.field(p)) {
            Some(n) => v = n,
            None => return 0,
        }
    }
    v.items().len()
}

fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let files = ix.find("*.ymap", &Filter::default(), false, usize::MAX);
    // (stored, predicted) -> count, per content bit; and per flags bit 1
    let mut tab: BTreeMap<(&str, bool, bool), usize> = BTreeMap::new();
    // for HD bit disagreements: what else the map holds
    let mut hd_extra: BTreeMap<String, usize> = BTreeMap::new();
    let mut n = 0usize;
    for r in files.iter() {
        let Ok(data) = game.read(&r.path) else { continue };
        let Ok(doc) = YmapDocument::open(&data, None) else { continue };
        n += 1;
        let stored_c = doc.root.as_struct().and_then(|s| s.field("contentFlags")).and_then(|v| v.as_u32()).unwrap_or(0);
        let stored_f = doc.root.as_struct().and_then(|s| s.field("flags")).and_then(|v| v.as_u32()).unwrap_or(0);
        let (mut hd, mut lod, mut slod1, mut slod2p, mut mlo) = (false, false, false, false, false);
        for i in 0..doc.len() {
            let Some(e) = doc.entity(i) else { continue };
            if e.is_mlo_instance {
                mlo = true;
            }
            match e.lod_level {
                0 | 5 => hd = true,
                1 => lod = true,
                2 => slod1 = true,
                3 | 4 | 6 => slod2p = true,
                _ => {}
            }
        }
        let occl = count(&doc.root, &["boxOccluders"]) + count(&doc.root, &["occludeModels"]) > 0;
        let phys = count(&doc.root, &["physicsDictionaries"]) > 0;
        let lodl = count(&doc.root, &["LODLightsSOA", "direction"]) > 0;
        let dist = count(&doc.root, &["DistantLODLightsSOA", "position"]) > 0;
        let grass = count(&doc.root, &["instancedData", "GrassInstanceList"]) > 0;
        let props = count(&doc.root, &["instancedData", "PropInstanceList"]) > 0;
        let cargens = count(&doc.root, &["carGenerators"]) > 0;
        let preds: [(&str, u32, bool); 10] = [
            ("c0 HD", 0, hd),
            ("c1 LOD", 1, lod),
            ("c2 SLOD2+", 2, slod2p),
            ("c3 Interior", 3, mlo),
            ("c4 SLOD", 4, slod1 || slod2p),
            ("c5 Occlusion", 5, occl),
            ("c6 Physics", 6, phys),
            ("c7 LODlights", 7, lodl),
            ("c8 DistantLights", 8, dist),
            ("c10 Grass", 10, grass),
        ];
        for (name, bit, p) in preds {
            *tab.entry((name, stored_c & (1 << bit) != 0, p)).or_default() += 1;
        }
        let flag1 = lod || slod1 || slod2p || dist;
        *tab.entry(("f1 LOD ymap", stored_f & 2 != 0, flag1)).or_default() += 1;
        if stored_c & 1 != 0 && !hd {
            let mut what = vec![];
            if props { what.push("props"); }
            if grass { what.push("grass"); }
            if cargens { what.push("cargens"); }
            if occl { what.push("occluders"); }
            if doc.len() > 0 { what.push("other-level entities"); }
            if what.is_empty() { what.push("nothing else"); }
            *hd_extra.entry(what.join("+")).or_default() += 1;
        }
    }
    println!("{n} maps");
    let mut last = "";
    for ((name, s, p), c) in &tab {
        if *name != last {
            println!();
            last = name;
        }
        println!("{name:18} stored {s:5} rule {p:5}: {c}");
    }
    println!("\nmaps with the HD bit stored but no HD entity hold: {hd_extra:?}");
    Ok(())
}
