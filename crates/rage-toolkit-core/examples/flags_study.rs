//! Which entities make the game's tools set which `contentFlags` bits? Tests simple rules on every `.ymap` of the game.
//! Bits: 0 HD, 1 LOD, 2 SLOD2+, 3 Interior, 4 SLOD (the names `YmapHeader::CONTENT_FLAG_NAMES` gives).
use rage_toolkit_core::{game::Game, index::FileIndex, job::Job, worldload::WorldData};

fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let w = WorldData::load(&game, &ix, &Job::new())?;
    let mut start = 0;
    let mut maps = 0usize;
    // per bit: (stored 1 predicted 1, stored 0 predicted 1, stored 1 predicted 0, stored 0 predicted 0)
    let mut tab = [[0usize; 4]; 5];
    let mut examples: Vec<String> = vec![];
    while start < w.ents.len() {
        let map = w.ents[start].map;
        let end = start + w.ents[start..].iter().take_while(|e| e.map == map).count();
        let ents = &w.ents[start..end];
        start = end;
        let Ok(data) = game.read(&w.maps[map as usize]) else { continue };
        let Ok(h) = rage_formats::parse_ymap_header(&data) else { continue };
        maps += 1;
        let any = |f: &dyn Fn(&rage_toolkit_core::worldload::WorldEnt) -> bool| ents.iter().any(|e| f(e));
        let pred = [
            any(&|e| !e.is_mlo && (e.lod_level == 0 || e.lod_level == 5)),
            any(&|e| !e.is_mlo && e.lod_level == 1),
            any(&|e| !e.is_mlo && matches!(e.lod_level, 3 | 4 | 6)),
            any(&|e| e.is_mlo),
            any(&|e| !e.is_mlo && e.lod_level == 2),
        ];
        for (b, p) in pred.iter().enumerate() {
            let s = h.content_flags & (1 << b) != 0;
            tab[b][match (s, *p) { (true, true) => 0, (false, true) => 1, (true, false) => 2, (false, false) => 3 }] += 1;
            if s != *p && examples.len() < 6 {
                let lv: Vec<u8> = { let mut v: Vec<u8> = ents.iter().map(|e| e.lod_level).collect(); v.sort(); v.dedup(); v };
                examples.push(format!("bit {b} stored {s} predicted {p}: {} (content {:#b}, levels {lv:?})", w.maps[map as usize].rsplit('/').next().unwrap(), h.content_flags));
            }
        }
    }
    println!("{maps} maps with entities");
    for (b, t) in tab.iter().enumerate() {
        println!("bit {b} {:>8}: both {:6}, only predicted {:6}, only stored {:6}, neither {:6}", rage_formats::YmapHeader::CONTENT_FLAG_NAMES[b], t[0], t[1], t[2], t[3]);
    }
    for e in examples {
        println!("{e}");
    }
    Ok(())
}
