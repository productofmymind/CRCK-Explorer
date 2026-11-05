//! `dump_file <game path> <out file>`: copy a file out of the game's archives (for building test inputs).
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let ext = a[0].rsplit('.').next().unwrap().to_string();
    let r = ix.find(&a[0], &Filter { exts: vec![ext], ..Default::default() }, false, 5);
    let hit = r.iter().find(|r| r.name().eq_ignore_ascii_case(&a[0])).unwrap_or_else(|| { for x in &r { println!("candidate {}", x.path); } panic!("not found") });
    std::fs::write(&a[1], g.read(&hit.path).unwrap()).unwrap();
    println!("{} -> {}", hit.path, a[1]);
}
