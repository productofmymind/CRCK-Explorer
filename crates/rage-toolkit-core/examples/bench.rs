//! Timings of the slow paths on the user's own installed game (nothing is written outside a temporary folder).
//!
//! `cargo run --release -p rage-toolkit-core --example bench -- [vehicle]` (default `adder`). Needs GTA V installed; without it,
//! it says so and stops. Each line is one step; compare runs before and after a change on the same machine.
use rage_toolkit_core::{game::Game, model, search};
use std::time::Instant;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("{label:<44} {:>8.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

fn main() -> anyhow::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "adder".into());
    let Ok(game) = Game::open(None) else {
        println!("GTA V not found: nothing to measure");
        return Ok(());
    };
    let out = std::env::temp_dir().join(format!("rage_toolkit_bench_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let hits = time("find <name>.yft (walk every archive)", || game.find(&format!("{name}.yft")));
    println!("  {} hit(s)", hits.len());
    let q = search::Query::parse("*.ytd", search::Kind::Files).unwrap();
    let n = time("search *.ytd (walk + match)", || game.search(&q, usize::MAX).len());
    println!("  {n} match(es)");
    let files = time("extract_vehicle (cold: no chain cache)", || game.extract_vehicle(&name, &out))?;
    time("extract_vehicle (warm: chain cached)", || game.extract_vehicle(&name, &out))?;
    let model_path = files.iter().find(|f| f.extension().is_some_and(|e| e == "yft") && !f.to_string_lossy().ends_with("_hi.yft")).cloned().unwrap();
    let ytds: Vec<_> = files.iter().filter(|f| f.extension().is_some_and(|e| e == "ytd")).cloned().collect();
    let meshes = time("placed_meshes (parse the model)", || model::placed_meshes(&model_path, model::LodLevel::High))?;
    let tex = time("load_textures_sourced (parse the ytds)", || model::load_textures_sourced(Some(&model_path), &ytds))?;
    let used: std::collections::HashSet<String> = meshes.iter().flat_map(|m| [&m.diffuse, &m.bump, &m.spec]).flatten().map(|s| s.to_lowercase()).collect();
    let (mut px, mut used_px, mut panel_px) = (0u64, 0u64, 0u64);
    time("decode every texture to RGBA (top mip)", || {
        for (t, _) in &tex {
            if let Ok((w, h, _)) = model::texture_rgba(t) {
                px += w as u64 * h as u64;
                if used.contains(&t.name.to_lowercase()) {
                    used_px += w as u64 * h as u64;
                }
                // the viewer's Textures panel keeps a copy halved until its longest side is 512 or less
                let (mut pw, mut ph) = (w as u64, h as u64);
                while pw.max(ph) > 512 {
                    (pw, ph) = ((pw / 2).max(1), (ph / 2).max(1));
                }
                panel_px += pw * ph;
            }
        }
    });
    let mb = |p: u64| p as f64 * 4.0 / 1048576.0;
    println!("  {} textures, {:.1} MB as RGBA; the model uses {} of them, {:.1} MB", tex.len(), mb(px), used.len(), mb(used_px));
    // GPU memory of the viewer for this vehicle: 3D textures with a full mip chain (x 4/3) plus the panel's pictures
    println!("  viewer GPU textures: all uploaded + full-size panel = {:.0} MB; used only + panel <= 512 px = {:.0} MB", mb(px) * 4.0 / 3.0 + mb(px), mb(used_px) * 4.0 / 3.0 + mb(panel_px));
    let _ = std::fs::remove_dir_all(&out);
    Ok(())
}
