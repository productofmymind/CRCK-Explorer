//! The extents rules against the game's own `.ymap` files: for each entity the archetype box times
//! the scale, turned by the orientation and moved to the position; the streaming box is the same with the LOD distance added to the
//! box before it is turned. Prints how often each variant matches the stored values within 1 m.
use glam::{Quat, Vec3};
use rage_toolkit_core::{game::Game, index::FileIndex, job::Job, worldload::{WorldData, WorldEnt}};

struct Boxes {
    ent: (Vec3, Vec3),
    stream_a: (Vec3, Vec3),
    stream_b: (Vec3, Vec3),
}

fn entity_boxes(w: &WorldData, e: &WorldEnt) -> Option<Boxes> {
    let a = w.arch.get(&e.hash)?;
    let lod = w.lod_dist_of(e);
    let s = Vec3::new(e.scale_xy, e.scale_xy, e.scale_z);
    let q = Quat::from_xyzw(e.rot[0], e.rot[1], e.rot[2], e.rot[3]);
    let rot = if q.length_squared() > 0.0 { q.normalize().conjugate() } else { Quat::IDENTITY };
    let p = Vec3::from(e.pos);
    let (lo, hi) = (Vec3::from(a.bb_min), Vec3::from(a.bb_max));
    let fold = |lo: Vec3, hi: Vec3| {
        let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for i in 0..8 {
            let c = Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
            let v = p + rot * c;
            mn = mn.min(v);
            mx = mx.max(v);
        }
        (mn, mx)
    };
    let l = Vec3::splat(lod);
    Some(Boxes { ent: fold(lo * s, hi * s), stream_a: fold(lo * s - l, hi * s + l), stream_b: fold((lo - l) * s, (hi + l) * s) })
}

fn near(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> bool {
    (a.0 - b.0).abs().max_element() < 1.0 && (a.1 - b.1).abs().max_element() < 1.0
}

fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let w = WorldData::load(&game, &ix, &Job::new())?;
    let (mut start, mut n) = (0, 0usize);
    let (mut ent_ok, mut a_ok, mut b_ok) = (0, 0, 0);
    let mut shown = 0;
    while start < w.ents.len() {
        let map = w.ents[start].map;
        let end = start + w.ents[start..].iter().take_while(|e| e.map == map).count();
        let ents = &w.ents[start..end];
        start = end;
        let all: Option<Vec<Boxes>> = ents.iter().map(|e| if e.is_mlo { None } else { entity_boxes(&w, e) }).collect();
        let Some(all) = all else { continue };
        let Ok(data) = game.read(&w.maps[map as usize]) else { continue };
        let Ok(h) = rage_formats::parse_ymap_header(&data) else { continue };
        let v = |p: rage_formats::Vec3| Vec3::new(p.x, p.y, p.z);
        let (se, ss) = ((v(h.entities_extents_min), v(h.entities_extents_max)), (v(h.streaming_extents_min), v(h.streaming_extents_max)));
        let fold = |f: &dyn Fn(&Boxes) -> (Vec3, Vec3)| all.iter().map(f).fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |a, b| (a.0.min(b.0), a.1.max(b.1)));
        n += 1;
        let (e, a, b) = (fold(&|x| x.ent), fold(&|x| x.stream_a), fold(&|x| x.stream_b));
        ent_ok += near(e, se) as i32;
        a_ok += near(a, ss) as i32;
        b_ok += near(b, ss) as i32;
        if !near(a, ss) && !near(b, ss) && shown < 3 {
            shown += 1;
            println!("e.g. {}: stored stream {:?}, A {:?}, B {:?}", w.maps[map as usize], ss, a, b);
        }
    }
    println!("{n} maps with only entities of known archetypes");
    println!("entities box matches: {ent_ok}; streaming A (lod added before the scale... after: lo*s-l) {a_ok}; streaming B ((lo-l)*s) {b_ok}");
    Ok(())
}
