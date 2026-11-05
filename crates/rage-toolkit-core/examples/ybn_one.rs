use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ybn_doc::YbnDocument};
fn main() {
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let f = Filter { exts: vec!["ybn".into()], ..Default::default() };
    let r = ix.find("v_janitor", &f, false, 5);
    let b = g.read(&r[0].path).unwrap();
    let mut d = YbnDocument::open(&b).unwrap();
    println!("geoms {}", d.geometry_count());
    for k in 0..d.geometry_count() { let v = d.vertices(k).unwrap(); println!("geom {k}: {} verts, first {:?}, polys {}", v.len(), v.first(), d.polygons(k).unwrap().len()); }
    d.set_vertex(0, 0, [3.3689091, -0.8329322, -1.3373231]).unwrap();
    let back = YbnDocument::open(&d.to_bytes().unwrap()).unwrap();
    for k in 0..back.geometry_count() { let v = back.world_vertices(k).unwrap(); println!("after {k}: {} verts, first world {:?}; before world {:?}", v.len(), v.first(), d.world_vertices(k).unwrap().first()); }
    let x = d.xml(); let a = x.find("<Vertices").unwrap(); let st = x[..a].rfind("<Item type").unwrap(); println!("EDITED {}", &x[st..a]);
    let y = rage_formats::dump_ybn_xml(&d.to_bytes().unwrap()).unwrap(); let a = y.find("<Vertices").unwrap(); let st = y[..a].rfind("<Item type").unwrap(); println!("REBUILT {}", &y[st..a]);
}
