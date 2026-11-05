//! How does the game's tooling fill `entitiesExtents` and `streamingExtents`? Tries a few formulas on every `.ymap` of the game and says how
//! often each one matches the stored values (within 1 m). `cargo run --release -p rage-toolkit-core --example extents_study`
use glam::{Quat, Vec3};
use rage_toolkit_core::{game::Game, index::FileIndex, job::Job, worldload::{WorldData, WorldEnt}};

fn corners(e: &WorldEnt, lo: Vec3, hi: Vec3) -> [Vec3; 8] {
    let s = Vec3::new(e.scale_xy, e.scale_xy, e.scale_z);
    let q = Quat::from_xyzw(e.rot[0], e.rot[1], e.rot[2], e.rot[3]);
    let rot = if q.length_squared() > 0.0 { q.normalize().conjugate() } else { Quat::IDENTITY };
    let p = Vec3::from(e.pos);
    std::array::from_fn(|i| p + rot * (Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z }) * s))
}

fn union(boxes: impl Iterator<Item = (Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
    boxes.fold(None, |a, (lo, hi)| Some(match a { None => (lo, hi), Some((l, h)) => (l.min(lo), h.max(hi)) }))
}

fn near(a: Option<(Vec3, Vec3)>, b: (Vec3, Vec3)) -> bool {
    a.map_or(false, |(lo, hi)| (lo - b.0).abs().max_element() < 1.0 && (hi - b.1).abs().max_element() < 1.0)
}

fn main() -> anyhow::Result<()> {
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let w = WorldData::load(&game, &ix, &Job::new())?;
    // entities are stored map after map
    let mut start = 0;
    let (mut n, mut ent_a, mut ent_b, mut ent_c) = (0, 0, 0, 0);
    let (mut s1, mut s2, mut s3, mut s4, mut s5, mut s6, mut s7, mut s8) = (0, 0, 0, 0, 0, 0, 0, 0);
    while start < w.ents.len() {
        let map = w.ents[start].map;
        let end = start + w.ents[start..].iter().take_while(|e| e.map == map).count();
        let ents = &w.ents[start..end];
        start = end;
        if ents.iter().any(|e| e.is_mlo || !w.arch.contains_key(&e.hash)) {
            continue;
        }
        let Ok(data) = game.read(&w.maps[map as usize]) else { continue };
        let Ok(h) = rage_formats::parse_ymap_header(&data) else { continue };
        let stored_e = (Vec3::new(h.entities_extents_min.x, h.entities_extents_min.y, h.entities_extents_min.z), Vec3::new(h.entities_extents_max.x, h.entities_extents_max.y, h.entities_extents_max.z));
        let stored_s = (Vec3::new(h.streaming_extents_min.x, h.streaming_extents_min.y, h.streaming_extents_min.z), Vec3::new(h.streaming_extents_max.x, h.streaming_extents_max.y, h.streaming_extents_max.z));
        n += 1;
        let bb = |e: &WorldEnt| {
            let a = &w.arch[&e.hash];
            (Vec3::from(a.bb_min), Vec3::from(a.bb_max))
        };
        let aabb = |e: &WorldEnt| {
            let (lo, hi) = bb(e);
            let c = corners(e, lo, hi);
            (c.iter().fold(Vec3::splat(f32::MAX), |a, b| a.min(*b)), c.iter().fold(Vec3::splat(f32::MIN), |a, b| a.max(*b)))
        };
        let sphere = |e: &WorldEnt| {
            let (lo, hi) = bb(e);
            let a = corners(e, lo, hi);
            let centre = a.iter().fold(Vec3::ZERO, |s, c| s + *c) / 8.0;
            let r = (hi - lo).length() * 0.5 * e.scale_xy.max(e.scale_z);
            (centre - Vec3::splat(r), centre + Vec3::splat(r))
        };
        let (ea, eb, ec) = (union(ents.iter().map(aabb)), union(ents.iter().map(|e| (Vec3::from(e.pos), Vec3::from(e.pos)))), union(ents.iter().map(sphere)));
        ent_a += near(ea, stored_e) as i32;
        ent_b += near(eb, stored_e) as i32;
        ent_c += near(ec, stored_e) as i32;
        let eff = |e: &WorldEnt| w.lod_dist_of(e);
        let maxlod = ents.iter().map(|e| eff(e)).fold(0.0f32, f32::max);
        let grow = |b: Option<(Vec3, Vec3)>, d: f32| b.map(|(l, h)| (l - Vec3::splat(d), h + Vec3::splat(d)));
        s1 += near(grow(ea, maxlod), stored_s) as i32;
        s2 += near(union(ents.iter().map(|e| { let (l, h) = aabb(e); (l - Vec3::splat(eff(e)), h + Vec3::splat(eff(e))) })), stored_s) as i32;
        s3 += near(union(ents.iter().map(|e| (Vec3::from(e.pos) - Vec3::splat(eff(e)), Vec3::from(e.pos) + Vec3::splat(eff(e))))), stored_s) as i32;
        s4 += near(union(ents.iter().map(|e| { let (l, h) = sphere(e); (l - Vec3::splat(e.lod_dist), h + Vec3::splat(e.lod_dist)) })), stored_s) as i32;
        s7 += near(union(ents.iter().map(|e| { let r = eff(e).max(e.child_lod_dist); (Vec3::from(e.pos) - Vec3::splat(r), Vec3::from(e.pos) + Vec3::splat(r)) })), stored_s) as i32;
        s8 += near(union(ents.iter().map(|e| { let r = if e.num_children > 0 && e.child_lod_dist > 0.0 { e.child_lod_dist } else { eff(e) }; (Vec3::from(e.pos) - Vec3::splat(r), Vec3::from(e.pos) + Vec3::splat(r)) })), stored_s) as i32;
        s5 += near(grow(ec, maxlod), stored_s) as i32;
        s6 += near(grow(eb, maxlod), stored_s) as i32;
        if n % 700 == 5 {
            let lods: Vec<f32> = { let mut v: Vec<f32> = ents.iter().map(|e| e.lod_dist).collect(); v.sort_by(|a, b| a.total_cmp(b)); v.dedup(); v };
            println!("{}  gap min {:?} max {:?}  lod dists {:?}", w.maps[map as usize].rsplit('/').next().unwrap(), stored_e.0 - stored_s.0, stored_s.1 - stored_e.1, &lods[..lods.len().min(6)]);
        }
    }
    println!("{n} maps with only known, non-interior entities");
    println!("entitiesExtents: boxes {ent_a}, positions {ent_b}, spheres {ent_c}");
    println!("streamingExtents: entities+maxLod {s1}, per-entity box+lod {s2}, positions+lod {s3}, per-entity sphere+lod {s4}, sphere extents+maxLod {s5}, positions+maxLod {s6}, positions+max(lod,child) {s7}, positions+child-if-parent {s8}");
    Ok(())
}
