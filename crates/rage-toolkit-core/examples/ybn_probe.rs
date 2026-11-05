use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, worldextra};
fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let ybns = ix.find("*", &Filter { exts: vec!["ybn".into()], ..Default::default() }, false, 40);
    for r in ybns.iter().step_by(7).take(5) {
        let bytes = game.read(&r.path)?;
        let t = std::time::Instant::now();
        match rage_formats::ybn::parse_ybn(&bytes) {
            Ok(y) => {
                let tris = y.triangles().len();
                let parts = worldextra::collision_parts(&bytes).map(|p| p.iter().map(|x| x.indices.len() / 3).sum::<usize>());
                println!("{}: root {:?} box {:?}..{:?}, {tris} triangles, parts {:?}, {:?}", r.name(), y.root.kind, y.root.box_min, y.root.box_max, parts.map_err(|e| e.to_string()), t.elapsed());
            }
            Err(e) => println!("{}: parse error {e:#}", r.name()),
        }
    }
    Ok(())
}
