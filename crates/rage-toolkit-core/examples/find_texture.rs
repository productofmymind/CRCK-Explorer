//! Which texture dictionaries of the game hold a texture? `cargo run --release -p rage-toolkit-core --example find_texture -- im_kerbs03 im_sidewalk010`
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, worldmodel::Ytd};

fn main() -> anyhow::Result<()> {
    let want: Vec<String> = std::env::args().skip(1).map(|s| s.to_lowercase()).collect();
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let ytds = ix.find("*", &Filter { exts: vec!["ytd".into()], ..Default::default() }, false, usize::MAX);
    println!("{} .ytd files", ytds.len());
    let mut found = vec![];
    for r in ytds {
        let Ok(bytes) = game.read(&r.path) else { continue };
        let Ok(y) = Ytd::parse(&bytes) else { continue };
        for w in &want {
            if y.has(w) {
                found.push(format!("{w}: {}", r.path));
            }
        }
    }
    found.sort();
    for f in found.iter().take(40) {
        println!("{f}");
    }
    Ok(())
}
