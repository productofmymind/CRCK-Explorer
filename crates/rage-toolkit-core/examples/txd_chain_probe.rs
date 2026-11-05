use rage_toolkit_core::{game::{txd_chain, Game}, index::{FileIndex, Filter}, job::Job, worldload::WorldData, worldmodel};
use std::collections::HashMap;
fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let p = worldmodel::txd_parents(&game, &ix);
    let w = WorldData::load(&game, &ix, &Job::new())?;
    let mut names: HashMap<u32, String> = HashMap::new();
    for r in ix.find("*", &Filter { exts: vec!["ydr".into(), "ytd".into(), "yft".into()], ..Default::default() }, false, usize::MAX) {
        let stem = r.name().rsplit_once('.').map_or(r.name(), |(s, _)| s).to_lowercase();
        names.insert(rage_toolkit_core::rage_joaat(&stem), stem);
    }
    println!("{} parent relations", p.len());
    for n in std::env::args().skip(1) {
        let h = rage_toolkit_core::rage_joaat(&n.to_lowercase());
        match w.arch.get(&h) {
            Some(a) => {
                let txd = names.get(&a.txd_hash).cloned().unwrap_or(format!("hash_{:08X}", a.txd_hash));
                println!("{n}: txd {txd}, chain {:?}", txd_chain(&p, &txd));
            }
            None => println!("{n}: no archetype"),
        }
    }
    Ok(())
}
