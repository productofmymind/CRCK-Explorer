//! Prints the XML of the first lines of one game `.ymap`: `cargo run --release -p rage-toolkit-core --example ymap_probe -- <name or path> [lines]`
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job};
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let hits = ix.find(&a[0], &Filter::default(), false, 1);
    let r = hits.first().ok_or_else(|| anyhow::anyhow!("not found"))?;
    println!("{}", r.path);
    let d = rage_formats::dump_meta(&game.read(&r.path)?)?;
    let xml = rage_formats::to_xml(&d.root, &rage_formats::NameTable::core());
    for l in xml.lines().take(a.get(1).and_then(|n| n.parse().ok()).unwrap_or(80)) {
        println!("{l}");
    }
    Ok(())
}
