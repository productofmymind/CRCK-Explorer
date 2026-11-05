use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job};
fn main() {
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let f = Filter { exts: vec!["ydr".into()], ..Default::default() };
    let (mut files, mut with) = (0, 0);
    for pat in ["lamp", "streetlight", "light"] {
        for r in ix.find(pat, &f, false, 150) {
            let Ok(b) = g.read(&r.path) else { continue };
            files += 1;
            if let Ok(l) = rage_formats::drawable_lights::parse_ydr_lights(&b) {
                if !l.is_empty() {
                    with += 1;
                    if with <= 3 { println!("{} {} lights, first {:?}", r.name(), l.len(), l[0]); }
                }
            }
        }
    }
    println!("{with} of {files} models have lights");
}
