//! Timing and memory of the map-editing operations at 100, 1,000 and 10,000 entities:
//! `cargo run --release -p rage-toolkit-core --example ymap_bench`
//! Prints a table (milliseconds, and bytes of live heap measured with a counting allocator). Nothing is written to disk.
use rage_toolkit_core::{
    ymap_doc::{ArchBounds, ExtentsMode, Vec3, YmapDocument},
    ymap_ops::Placement,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
    time::Instant,
};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(n, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                let n = LIVE.fetch_add(new - l.size(), Relaxed) + (new - l.size());
                PEAK.fetch_max(n, Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn bounds(_: u32) -> Option<ArchBounds> {
    Some(ArchBounds { min: [-1.0, -1.0, 0.0], max: [1.0, 1.0, 2.0], lod_dist: 100.0 })
}

fn place(i: usize) -> Placement {
    let (x, y) = ((i % 100) as f32 * 5.0, (i / 100) as f32 * 5.0);
    Placement { position: Vec3::new(x, y, -3.0), rotation: [0.0, 0.0, 0.3827, 0.9239], scale_xy: 1.0, scale_z: 1.0 }
}

struct Row {
    n: usize,
    cols: Vec<(&'static str, String)>,
}

fn run(n: usize) -> Row {
    let mut cols: Vec<(&'static str, String)> = vec![];
    let mut put = |k: &'static str, v: String| cols.push((k, v));
    let base = LIVE.load(Relaxed);
    let mut d = YmapDocument::new_map("bench").unwrap();
    // adding one by one, as the editor does
    let t = Instant::now();
    for i in 0..n {
        d.add_entity_at("prop_bench_01a", place(i)).unwrap();
    }
    let add_total = ms(t);
    put("add all (ms)", format!("{add_total:.1}"));
    put("add one (ms)", format!("{:.3}", add_total / n as f64));
    let live = LIVE.load(Relaxed) - base;
    put("heap (MB)", format!("{:.1}", live as f64 / 1e6));
    put("heap/entity (B)", format!("{}", live / n));

    let t = Instant::now();
    let mut acc = 0.0f32;
    for i in 0..n {
        acc += d.entity(i).map_or(0.0, |e| e.position.x);
    }
    put("read all entities (ms)", format!("{:.2}", ms(t)));
    assert!(acc.is_finite());

    // a LOD hierarchy: the first quarter are LOD parents, each with up to three HD children after them
    let parents = (n / 4).max(1);
    let t = Instant::now();
    d.set_lod_levels(&(0..parents).map(|p| (p, 1)).collect::<Vec<_>>()).unwrap();
    d.set_parents(&(parents..n).map(|c| (c, Some((c - parents) % parents))).collect::<Vec<_>>()).unwrap();
    put("build LOD hierarchy, 2 steps (ms)", format!("{:.1}", ms(t)));
    // the same edit made one entity at a time (a few, to get the cost of one)
    let t = Instant::now();
    let few = 20.min(n - parents);
    for c in parents..parents + few {
        d.set_parent(c, None).unwrap();
        d.set_parent(c, Some((c - parents) % parents)).unwrap();
    }
    put("set_parent, one call (ms)", format!("{:.2}", ms(t) / (2 * few) as f64));

    let t = Instant::now();
    let h = d.hierarchy();
    put("hierarchy() (ms)", format!("{:.2}", ms(t)));
    assert_eq!(h.roots.len(), parents);
    let t = Instant::now();
    let r = d.rebuild_hierarchy(rage_toolkit_core::ymap_lod::CountMode::RaiseOnly).unwrap();
    put("rebuild_hierarchy (ms)", format!("{:.2}", ms(t)));
    assert!(r.detached.is_empty());

    let t = Instant::now();
    let ex = d.calc_extents(&bounds);
    put("calc_extents (ms)", format!("{:.2}", ms(t)));
    assert!(ex.is_some());

    let t = Instant::now();
    let _ = d.calc_flags();
    put("calc_flags (ms)", format!("{:.2}", ms(t)));

    let t = Instant::now();
    let _ = d.validate(Some(&|_| true), &bounds);
    put("validate (ms)", format!("{:.2}", ms(t)));

    let t = Instant::now();
    d.update_extents(&bounds, ExtentsMode::Recalculate).unwrap();
    d.update_flags().unwrap();
    put("update extents+flags (ms)", format!("{:.2}", ms(t)));

    // the cost of one frame of a drag: the same 100 (or all) entities set again, joined into one undo step
    let k = n.min(100);
    let frames = 200;
    let t = Instant::now();
    for f in 0..frames {
        let list: Vec<(usize, Placement)> = (0..k)
            .map(|i| {
                let mut p = place(i);
                p.position.x += f as f32 * 0.01;
                (i, p)
            })
            .collect();
        d.set_placements(&list, Some("drag".into())).unwrap();
    }
    put("drag frame, 100 entities (ms)", format!("{:.3}", ms(t) / frames as f64));
    let t = Instant::now();
    let list: Vec<(usize, Placement)> = (0..n).map(|i| (i, place(i))).collect();
    d.set_placements(&list, None).unwrap();
    put("move all at once (ms)", format!("{:.1}", ms(t)));

    // duplicate / paste a tenth of the entities
    let k = (n / 10).max(1);
    let t = Instant::now();
    let items = d.copy_entities(&(0..k).collect::<Vec<_>>());
    d.paste_entities(&items, Vec3::new(1.0, 0.0, 0.0)).unwrap();
    put("paste 10% in one step (ms)", format!("{:.2}", ms(t)));
    let t = Instant::now();
    for i in 0..k {
        d.duplicate_entity(i).unwrap();
    }
    put("duplicate 10% one by one (ms)", format!("{:.1}", ms(t)));

    // remove the entities just added
    let total = d.len();
    let t = Instant::now();
    let doomed: Vec<usize> = (n..total).collect();
    d.remove_entities(&doomed).unwrap();
    put("remove 20% in one step (ms)", format!("{:.2}", ms(t)));
    let t = Instant::now();
    let mut removed = 0;
    for i in (n.saturating_sub(k)..n).rev() {
        if d.remove_entity(i).is_ok() {
            removed += 1;
        }
    }
    put("remove 10% one by one (ms)", format!("{:.1} ({removed} ok)", ms(t)));

    // undo everything the run did to the entity list (the history walks back)
    let t = Instant::now();
    let mut steps = 0;
    while d.can_undo() && steps < 50 {
        d.undo().unwrap();
        steps += 1;
    }
    put("undo 50 steps (ms)", format!("{:.2}", ms(t)));
    while d.can_redo() {
        d.redo().unwrap();
    }

    let before_peak = PEAK.load(Relaxed);
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let bytes = d.to_bytes().unwrap();
    put("save: build + verify (ms)", format!("{:.1}", ms(t)));
    put("file (KB)", format!("{}", bytes.len() / 1024));
    put("save peak extra (MB)", format!("{:.1}", (PEAK.load(Relaxed) - LIVE.load(Relaxed)) as f64 / 1e6));
    let _ = before_peak;
    let t = Instant::now();
    let reopened = YmapDocument::open(&bytes, None).unwrap();
    put("open (ms)", format!("{:.1}", ms(t)));
    assert_eq!(reopened.len(), d.len());
    Row { n, cols }
}

fn main() {
    let sizes: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let sizes = if sizes.is_empty() { vec![100, 1000, 10000] } else { sizes };
    let rows: Vec<Row> = sizes.into_iter().map(run).collect();
    print!("{:<34}", "entities");
    for r in &rows {
        print!("{:>16}", r.n);
    }
    println!();
    for k in 0..rows[0].cols.len() {
        print!("{:<34}", rows[0].cols[k].0);
        for r in &rows {
            print!("{:>16}", r.cols[k].1);
        }
        println!();
    }
}
