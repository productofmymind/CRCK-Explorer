//! Does every `.ymap` of the user's game survive read, write, read? Uses the same check as the archetype editor.
//! `cargo run --release -p rage-toolkit-core --example ymap_fidelity [-- --dir <game folder>] [--limit N]`
use rage_toolkit_core::{archetype::fidelity, game::Game, index::{FileIndex, Filter}, job::Job};
use std::collections::BTreeMap;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let game = Game::open(arg("--dir").as_deref().map(std::path::Path::new))?;
    let limit: usize = arg("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let files = ix.find("*.ymap", &Filter::default(), false, usize::MAX);
    println!("{} .ymap files in the index", files.len());
    let (mut ok, mut lossy, mut failed) = (0usize, 0usize, 0usize);
    let mut why: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for r in files.iter().take(limit) {
        let data = match game.read(&r.path) {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                why.entry(format!("read: {e:#}").chars().take(80).collect()).or_insert((0, r.path.clone())).0 += 1;
                continue;
            }
        };
        match fidelity::check(&data) {
            Ok(f) if f.is_lossless() => ok += 1,
            Ok(f) => {
                lossy += 1;
                let key = f.differences.first().map(|d| format!("diff at {}", d.path.split('[').next().unwrap_or(""))).or_else(|| f.read_warnings.first().cloned()).or_else(|| f.write_warnings.first().cloned()).unwrap_or_default();
                why.entry(key.chars().take(100).collect()).or_insert((0, r.path.clone())).0 += 1;
            }
            Err(e) => {
                failed += 1;
                why.entry(format!("{e:#}").chars().take(100).collect()).or_insert((0, r.path.clone())).0 += 1;
            }
        }
    }
    println!("lossless {ok}, changed {lossy}, failed {failed}");
    for (k, (n, example)) in &why {
        println!("{n:6}  {k}\n        e.g. {example}");
    }
    Ok(())
}
