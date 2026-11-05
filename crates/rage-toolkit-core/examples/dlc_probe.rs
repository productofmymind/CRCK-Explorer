use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job};
fn main() {
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let f = Filter { exts: vec!["xml".into()], ..Default::default() };
    let r = ix.find("setup2.xml", &f, false, 400);
    println!("{} setup2.xml", r.len());
    for x in r.iter().take(5) { println!("{}", x.path); }
    if let Some(x) = r.first() { let b = g.read(&x.path).unwrap(); println!("{}", String::from_utf8_lossy(&b[..b.len().min(900)])); }
}
