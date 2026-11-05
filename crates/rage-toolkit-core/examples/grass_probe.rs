use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ymap_doc::YmapDocument};
fn main() {
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let f = Filter { exts: vec!["ymap".into()], ..Default::default() };
    for r in ix.find("lodlights", &f, false, 100) {
        let Ok(b) = g.read(&r.path) else { continue };
        let Ok(d) = YmapDocument::open(&b, None) else { continue };
        let xml = d.to_xml();
        if let Some(at) = xml.find("<LODLightsSOA") {
            println!("{}", r.path);
            println!("{}", &xml[at..(at + 1800).min(xml.len())]);
            if let Some(a2) = xml.find("<DistantLODLightsSOA") { println!("{}", &xml[a2..(a2 + 900).min(xml.len())]); }
            return;
        }
    }
}
