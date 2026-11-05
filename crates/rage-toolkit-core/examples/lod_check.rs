//! The LOD hierarchy rules against every `.ymap` of the game: how many maps have which kind of problem.
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ymap_doc::YmapDocument};
use std::collections::BTreeMap;
fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let (mut n, mut roots, mut ents, mut children) = (0usize, 0usize, 0usize, 0usize);
    let mut by: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let t = std::time::Instant::now();
    for r in ix.find("*.ymap", &Filter::default(), false, usize::MAX) {
        let Ok(data) = game.read(&r.path) else { continue };
        let Ok(doc) = YmapDocument::open(&data, None) else { continue };
        let h = doc.hierarchy();
        n += 1;
        ents += doc.len();
        roots += h.roots.len();
        children += h.parent.iter().flatten().count();
        for i in h.issues {
            let e = by.entry(format!("{:?}", i.kind)).or_insert((0, 0, r.path.clone()));
            e.0 += 1;
        }
        let mut seen: Vec<String> = doc.hierarchy().issues.iter().map(|i| format!("{:?}", i.kind)).collect();
        seen.sort();
        seen.dedup();
        for k in seen {
            by.get_mut(&k).unwrap().1 += 1;
        }
    }
    println!("{n} maps, {ents} entities: {roots} roots, {children} with a valid in-file parent ({:.1} s)", t.elapsed().as_secs_f32());
    for (k, (issues, maps, ex)) in by {
        println!("{k:22} {issues:8} entities in {maps:6} maps   e.g. {ex}");
    }
    Ok(())
}
