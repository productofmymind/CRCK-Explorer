//! Loads the whole map of the user's game and says how long it took and what is in it.
use rage_toolkit_core::{game::Game, index::FileIndex, job::Job, worldload::WorldData};
fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let t = std::time::Instant::now();
    let w = WorldData::load(&game, &ix, &Job::new())?;
    println!("{} maps, {} entities, {} archetypes with sizes from {} .ytyp, {} files skipped, {:.1} s", w.maps.len(), w.ents.len(), w.arch.len(), w.ytyps.len(), w.skipped.len(), t.elapsed().as_secs_f32());
    for s in w.skipped.iter().take(5) {
        println!("  skipped: {s}");
    }
    let known = w.ents.iter().filter(|e| w.arch.contains_key(&e.hash)).count();
    println!("{} of {} entities have a known archetype size", known, w.ents.len());
    println!("{} entities in the 3x3 cells around Legion Square (200,-900)", w.near(200.0, -900.0, 300.0).iter().map(|c| c.len()).sum::<usize>());
    let (mut with_kids, mut no_kids) = (0, 0);
    let mut sample = vec![];
    for c in w.near(-1100.0, -250.0, 600.0) {
        for &i in c {
            let e = &w.ents[i as usize];
            if e.lod_level >= 2 && e.lod_level != 5 {
                if e.num_children > 0 { with_kids += 1 } else { no_kids += 1; if sample.len() < 4 { sample.push((e.lod_level, e.lod_dist, e.child_lod_dist, e.num_children, w.maps[e.map as usize].clone())); } }
            }
        }
    }
    println!("SLOD near (-1100,-250): {with_kids} with children, {no_kids} without; samples {sample:?}");
    Ok(())
}
