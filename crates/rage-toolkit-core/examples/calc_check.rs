//! The extents and flag rules (`YmapDocument::calc_extents` / `calc_flags`) against every `.ymap` of the game.
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, worldload::WorldData, ymap_calc::MANAGED_CONTENT_BITS, ymap_doc::{ArchBounds, YmapDocument}};
fn near(a: [f32; 3], b: rage_formats::Vec3) -> bool {
    (a[0] - b.x).abs() < 1.0 && (a[1] - b.y).abs() < 1.0 && (a[2] - b.z).abs() < 1.0
}
fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let w = WorldData::load(&game, &ix, &Job::new())?;
    let bounds = |h: u32| w.arch.get(&h).map(|a| ArchBounds { min: a.bb_min, max: a.bb_max, lod_dist: a.lod_dist });
    let files = ix.find("*.ymap", &Filter::default(), false, usize::MAX);
    let (mut n, mut ent_ok, mut str_ok, mut str_big, mut flags_ok, mut empty_ent) = (0, 0, 0, 0, 0, 0);
    let mut shown = 0;
    for r in files.iter() {
        let Ok(data) = game.read(&r.path) else { continue };
        let Ok(doc) = YmapDocument::open(&data, None) else { continue };
        let Ok(h) = rage_formats::parse_ymap_header(&data) else { continue };
        n += 1;
        let (sf, sc) = (h.flags, h.content_flags);
        let (cf, cc) = doc.calc_flags();
        flags_ok += (((sc ^ cc) & MANAGED_CONTENT_BITS) == 0 && ((sf ^ cf) & 2) == 0) as i32;
        // only maps whose entities are all of known archetypes can be compared
        if (0..doc.len()).any(|i| doc.entity(i).map_or(false, |e| !e.is_mlo_instance && bounds(e.archetype_hash).is_none())) {
            continue;
        }
        match doc.calc_extents(&bounds) {
            None => empty_ent += 1,
            Some(e) => {
                let ok = near(e.entities_min, h.entities_extents_min) && near(e.entities_max, h.entities_extents_max);
                ent_ok += ok as i32;
                let s_ok = near(e.streaming_min, h.streaming_extents_min) && near(e.streaming_max, h.streaming_extents_max);
                str_ok += s_ok as i32;
                // streaming at least as big as the stored one: safe even where it does not match
                let ge = |a: [f32; 3], b: rage_formats::Vec3, sign: f32| sign * (a[0] - b.x) >= -1.0 && sign * (a[1] - b.y) >= -1.0 && sign * (a[2] - b.z) >= -1.0;
                str_big += (ge(e.streaming_max, h.streaming_extents_max, 1.0) && ge(e.streaming_min, h.streaming_extents_min, -1.0)) as i32;
                if !ok && shown < 3 {
                    shown += 1;
                    println!("e.g. {}: stored {:?}..{:?}, calculated {:?}..{:?}", r.path, h.entities_extents_min, h.entities_extents_max, e.entities_min, e.entities_max);
                }
            }
        }
    }
    println!("{n} maps; flags agree on all managed bits: {flags_ok}");
    println!("comparable maps: entity extents match {ent_ok}, streaming extents match {str_ok}, streaming at least as large as stored {str_big} (maps with nothing to size: {empty_ent})");
    Ok(())
}
