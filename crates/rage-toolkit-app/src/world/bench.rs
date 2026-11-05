//! A benchmark of the World editor in the real app (`crck-explorer --world-view x,y,z --world-bench N[,model]`): it makes a map of N entities
//! round the camera, lets the models stream in, then measures frames while it drags 100 selected entities, flies the camera, and sits idle,
//! times the single operations (picking, box selection, duplicate, delete, save) and the memory the process holds, writes the numbers to
//! `crck_world_bench_N.txt` in the temp folder and quits. It works in a scratch project and never touches the user's.
use super::*;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// waiting for the models to arrive
    Warm,
    /// dragging 100 entities, one move per frame
    Drag,
    /// flying the camera
    Fly,
    /// nothing changes: the cost of an unchanged frame
    Idle,
}

pub(super) struct Bench {
    n: usize,
    phase: Phase,
    frame: u32,
    quiet: u32,
    last_frame: Instant,
    started: Instant,
    mem_start: usize,
    drag_edit: Vec<f32>,
    drag_scene: Vec<f32>,
    drag_frame: Vec<f32>,
    drag_ui: Vec<f32>,
    fly_scene: Vec<f32>,
    fly_gather: Vec<f32>,
    fly_gpu: Vec<f32>,
    fly_visited: Vec<f32>,
    fly_frame: Vec<f32>,
    fly_ui: Vec<f32>,
    idle_frame: Vec<f32>,
    idle_ui: Vec<f32>,
    lines: Vec<String>,
}

/// bytes of memory the process holds now (its working set)
#[cfg(windows)]
fn working_set() -> usize {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        rest: [usize; 6],
    }
    extern "system" {
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
        fn GetCurrentProcess() -> isize;
    }
    let mut c = Counters { cb: std::mem::size_of::<Counters>() as u32, page_faults: 0, peak_working_set: 0, working_set: 0, rest: [0; 6] };
    // SAFETY: `Counters` has the layout of the Win32 PROCESS_MEMORY_COUNTERS (two DWORDs and eight SIZE_Ts, 72 bytes on 64-bit Windows), `cb`
    // says so, and the pseudo-handle from GetCurrentProcess is always valid.
    unsafe {
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, std::mem::size_of::<Counters>() as u32) != 0 {
            return c.working_set;
        }
    }
    0
}

#[cfg(not(windows))]
fn working_set() -> usize {
    0
}

fn stats(v: &[f32]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let mean = s.iter().sum::<f32>() / s.len() as f32;
    format!("mean {mean:.2}, p95 {:.2}, max {:.2}", s[(s.len() * 95 / 100).min(s.len() - 1)], s[s.len() - 1])
}

fn ms_since(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

impl Bench {
    fn new(n: usize) -> Self {
        let now = Instant::now();
        Self {
            n,
            phase: Phase::Warm,
            frame: 0,
            quiet: 0,
            last_frame: now,
            started: now,
            mem_start: working_set(),
            drag_edit: vec![],
            drag_scene: vec![],
            drag_frame: vec![],
            drag_ui: vec![],
            fly_scene: vec![],
            fly_gather: vec![],
            fly_gpu: vec![],
            fly_visited: vec![],
            fly_frame: vec![],
            fly_ui: vec![],
            idle_frame: vec![],
            idle_ui: vec![],
            lines: vec![],
        }
    }
}

impl World {
    /// Ask for the benchmark once the map is loaded.
    pub fn queue_bench(&mut self, n: usize, model: Option<String>) {
        self.automation = true;
        self.pending_bench = Some((n.max(1), model.unwrap_or_else(|| "prop_bench_01a".into())));
    }

    /// N entities on a grid round the camera, each shown out to 1,500 m; the first 100 are selected.
    pub(super) fn build_bench(&mut self, n: usize, model: &str) {
        let t = Instant::now();
        let Some(slot) = self.new_map_in_project(Some("bench")) else { return };
        let centre = self.cam.pos;
        let side = (n as f32).sqrt().ceil() as usize;
        let spacing = 6.0f32;
        let place = |i: usize| {
            let (gx, gy) = ((i % side) as f32 - side as f32 / 2.0, (i / side) as f32 - side as f32 / 2.0);
            rage_toolkit_core::ymap_ops::Placement { position: rage_toolkit_core::ymap_doc::Vec3::new(centre.x + gx * spacing, centre.y + gy * spacing + 60.0, centre.z - 55.0), rotation: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 }
        };
        let name = model.to_string();
        self.doc_op(slot, |d| {
            d.add_entity_at(&name, place(0))?;
            d.set_entity_field(0, "lodDist", "1500")?;
            if n > 1 {
                let item = d.copy_entities(&[0]).remove(0);
                let items = vec![item; n - 1];
                d.paste_entities(&items, rage_toolkit_core::ymap_doc::Vec3::new(0.0, 0.0, 0.0))?;
                let list: Vec<(usize, rage_toolkit_core::ymap_ops::Placement)> = (0..n).map(|i| (i, place(i))).collect();
                d.set_placements(&list, None)?;
            }
            Ok(())
        });
        self.extra_names.insert(rage_toolkit_core::rage_joaat(&name.to_lowercase()), name);
        let first: Vec<u32> = (0..n.min(100)).map(|i| project::id(slot, i)).collect();
        self.set_selection(first);
        self.mode = ViewMode::Select;
        self.tool = Tool::Move;
        self.view_dist = 1500.0;
        self.layers.hd = true;
        self.clear_history();
        let mut b = Bench::new(n);
        b.lines.push(format!("entities in the map: {n} (built in {:.0} ms)", ms_since(t)));
        self.bench = Some(b);
    }

    /// One step of the benchmark, at the end of a frame.
    pub(super) fn bench_tick(&mut self, ctx: &egui::Context) {
        let Some(mut b) = self.bench.take() else { return };
        ctx.request_repaint();
        let frame_ms = ms_since(b.last_frame);
        b.last_frame = Instant::now();
        let scene_ms = self.scene.as_ref().map_or(0.0, |s| s.ms);
        let loading = self.loader.as_ref().map_or(true, |l| l.pending() > 0);
        match b.phase {
            Phase::Warm => {
                let drawn = self.scene.as_ref().map_or(0, |s| s.shown);
                if !loading && drawn > 0 {
                    b.quiet += 1;
                } else {
                    b.quiet = 0;
                }
                // give up waiting after a minute (the model may not exist)
                if b.quiet > 30 || b.started.elapsed().as_secs() > 90 {
                    b.lines.push(format!("drawn when ready: {drawn} instances, {} models loaded, waited {:.1} s", self.loader.as_ref().map_or(0, |l| l.loaded()), b.started.elapsed().as_secs_f32()));
                    b.phase = Phase::Drag;
                    b.frame = 0;
                }
            }
            Phase::Drag => {
                let t = Instant::now();
                let list: Vec<(u32, EntityEdit)> = self
                    .sel
                    .clone()
                    .into_iter()
                    .filter_map(|i| {
                        let mut p = self.placement_of(i)?;
                        p.pos[0] += 0.05;
                        Some((i, p))
                    })
                    .collect();
                self.set_placements(&list);
                b.drag_edit.push(ms_since(t));
                b.drag_scene.push(scene_ms);
                b.drag_frame.push(frame_ms);
                b.drag_ui.push(self.ui_ms);
                b.frame += 1;
                if b.frame >= 90 {
                    self.end_groups();
                    b.phase = Phase::Fly;
                    b.frame = 0;
                }
            }
            Phase::Fly => {
                self.cam.pos.x += 1.0;
                b.fly_scene.push(scene_ms);
                if let Some(s) = self.scene.as_ref() {
                    b.fly_gather.push(s.gather_ms);
                    b.fly_gpu.push(s.gpu_ms);
                    b.fly_visited.push(s.visited as f32);
                }
                b.fly_frame.push(frame_ms);
                b.fly_ui.push(self.ui_ms);
                b.frame += 1;
                if b.frame >= 60 {
                    b.phase = Phase::Idle;
                    b.frame = 0;
                }
            }
            Phase::Idle => {
                b.idle_frame.push(frame_ms);
                b.idle_ui.push(self.ui_ms);
                b.frame += 1;
                if b.frame >= 60 {
                    self.bench_finish(ctx, b);
                    return;
                }
            }
        }
        self.bench = Some(b);
    }

    /// The single operations, the memory, and the report.
    fn bench_finish(&mut self, ctx: &egui::Context, mut b: Bench) {
        let n = b.n;
        let mut line = |k: &str, v: String| b.lines.push(format!("{k}: {v}"));
        line("drag, 100 entities, edit (ms)", stats(&b.drag_edit));
        line("drag, scene build (ms)", stats(&b.drag_scene));
        line("drag, World ui() (ms)", stats(&b.drag_ui));
        line("drag, frame interval (ms)", stats(&b.drag_frame));
        line("camera fly, scene build (ms)", stats(&b.fly_scene));
        line("  of it choosing what to draw (ms)", stats(&b.fly_gather));
        line("  of it on the GPU side (ms)", stats(&b.fly_gpu));
        line("  entities looked at", stats(&b.fly_visited));
        line("camera fly, World ui() (ms)", stats(&b.fly_ui));
        line("camera fly, frame interval (ms)", stats(&b.fly_frame));
        line("idle, World ui() (ms)", stats(&b.idle_ui));
        line("idle frame interval (ms)", stats(&b.idle_frame));
        let (picks, shown) = self.scene.as_ref().map_or((0, 0), |s| (s.picks.len(), s.shown));
        line("picks / drawn instances in view", format!("{picks} / {shown}"));
        // picking by the mouse ray and by a box
        if let Some(scene) = self.scene.as_ref() {
            let (eye, fwd) = (self.cam.pos, self.cam.forward());
            let t = Instant::now();
            let mut hits = 0;
            for k in 0..200 {
                let d = (fwd + Vec3::new((k as f32 - 100.0) * 0.003, 0.0, ((k * 7 % 200) as f32 - 100.0) * 0.002)).normalize();
                hits += pick_ray(&scene.picks, eye, d).is_some() as usize;
            }
            line("pick by ray (ms per ray)", format!("{:.3} ({hits} of 200 hit)", ms_since(t) / 200.0));
            let rect = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0));
            let vp = self.cam.view_proj(rect.width() / rect.height());
            let t = Instant::now();
            let mut inside = 0;
            for _ in 0..20 {
                inside = picks_in_rect(&scene.picks, &vp, rect, rect).len();
            }
            line("box select, whole view (ms)", format!("{:.2} ({inside} inside)", ms_since(t) / 20.0));
        }
        // edits on the whole selection
        let slot = self.proj.active;
        let all: Vec<u32> = (0..n).map(|i| project::id(slot, i)).collect();
        let t = Instant::now();
        self.set_selection(all);
        line("select all (ms)", format!("{:.2}", ms_since(t)));
        let first: Vec<u32> = (0..n.min(100)).map(|i| project::id(slot, i)).collect();
        self.set_selection(first);
        let t = Instant::now();
        let copies = self.duplicate_selection(Vec3::new(1.0, 0.0, 0.0));
        line("duplicate 100 (ms)", format!("{:.2} ({} made)", ms_since(t), copies.len()));
        let t = Instant::now();
        self.delete_selection();
        line("delete 100 (ms)", format!("{:.2}", ms_since(t)));
        let t = Instant::now();
        self.proj_cache.valid = false;
        self.ensure_proj_cache();
        line("entity cache, rebuilt (ms)", format!("{:.2}", ms_since(t)));
        let t = Instant::now();
        self.struct_rev += 1;
        let rows = self.hier_rows(slot).rows_len();
        line("entity list and LOD tree, rebuilt (ms)", format!("{:.2} ({rows} rows)", ms_since(t)));
        let t = Instant::now();
        self.proj_rev += 1;
        let _ = self.calc_extents_cached(slot);
        line("extents, calculated (ms)", format!("{:.2}", ms_since(t)));
        let t = Instant::now();
        self.save_project(true);
        line("save the project, flags + extents + write + verify (ms)", format!("{:.1}", ms_since(t)));
        let now = working_set();
        line("process memory at the start / now (MB)", format!("{:.0} / {:.0}", b.mem_start as f64 / 1e6, now as f64 / 1e6));
        let text = b.lines.join("\n");
        let path = std::env::temp_dir().join(format!("crck_world_bench_{n}.txt"));
        let _ = std::fs::write(&path, &text);
        eprintln!("{text}");
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
