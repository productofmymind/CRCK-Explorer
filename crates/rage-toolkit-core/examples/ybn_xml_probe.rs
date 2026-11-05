use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ybn_doc::YbnDocument};
fn main() {
    let g = Game::open(None).unwrap();
    let ix = FileIndex::load_or_build(&g, &Job::new(), false).unwrap();
    let f = Filter { exts: vec!["ybn".into()], ..Default::default() };
    let (mut ok, mut bad, mut skipped) = (0, 0, 0);
    for x in ix.find("v_", &f, false, 120) {
        let Ok(b) = g.read(&x.path) else { continue };
        let Ok(mut d) = YbnDocument::open(&b) else { skipped += 1; continue };
        let Some(v0) = (0..d.geometry_count()).find_map(|k| d.world_vertices(k).ok().and_then(|v| v.first().copied().map(|p| (k, p)))) else { skipped += 1; continue };
        if d.set_vertex(v0.0, 0, [v0.1[0] + 1.0, v0.1[1], v0.1[2]]).is_err() { bad += 1; continue; }
        match d.to_bytes().and_then(|bytes| YbnDocument::open(&bytes)) {
            Ok(back) => { if back.world_vertices(v0.0).map(|v| v.iter().any(|p| (p[0] - (v0.1[0] + 1.0)).abs() < 0.05 && (p[1] - v0.1[1]).abs() < 0.05 && (p[2] - v0.1[2]).abs() < 0.05)).unwrap_or(false) { ok += 1 } else { bad += 1; println!("moved vertex lost in {}", x.path) } }
            Err(e) => { bad += 1; println!("{}: {e:#}", x.path) }
        }
    }
    println!("{ok} edited, saved and reread; {bad} failed; {skipped} skipped");
}
