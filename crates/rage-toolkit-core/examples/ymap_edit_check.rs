//! Every `.ymap` of the user's game through `YmapDocument`: save as is, move one entity, save, reopen, and compare; and how often
//! `check` complains (it should stay quiet on the game's own files).
//! `cargo run --release -p rage-toolkit-core --example ymap_edit_check [-- --limit N]`
use rage_formats::Vec3;
use rage_toolkit_core::{game::Game, index::{FileIndex, Filter}, job::Job, ymap_doc::YmapDocument};
use std::collections::BTreeMap;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let limit: usize = args.iter().position(|a| a == "--limit").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let game = Game::open(None)?;
    let ix = FileIndex::load_or_build(&game, &Job::new(), false)?;
    let files = ix.find("*.ymap", &Filter::default(), false, usize::MAX);
    let (mut files_n, mut with_entities, mut entities, mut save_fail, mut edit_fail, mut quiet) = (0, 0, 0usize, 0, 0, 0);
    let mut issues: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut failures: Vec<String> = vec![];
    for r in files.iter().take(limit) {
        let Ok(data) = game.read(&r.path) else { continue };
        let Ok(mut doc) = YmapDocument::open(&data, None) else {
            failures.push(format!("open {}", r.path));
            continue;
        };
        files_n += 1;
        if let Err(e) = doc.to_bytes() {
            save_fail += 1;
            failures.push(format!("save {}: {e:#}", r.path));
            continue;
        }
        let issue_list = doc.check(None);
        if issue_list.is_empty() {
            quiet += 1;
        }
        for i in issue_list {
            // the text without the numbers, so the same kind of finding counts together
            let kind: String = i.message.split_whitespace().filter(|w| !w.chars().any(|c| c.is_ascii_digit())).take(8).collect::<Vec<_>>().join(" ");
            issues.entry(kind).or_insert((0, r.path.clone())).0 += 1;
        }
        if doc.is_empty() {
            continue;
        }
        with_entities += 1;
        entities += doc.len();
        let before: Vec<_> = (0..doc.len()).map(|i| doc.entity(i)).collect();
        let moved = {
            let mut p = before[0].as_ref().map(|e| e.position).unwrap_or_default();
            p = Vec3::new(p.x + 1.0, p.y, p.z);
            p
        };
        let ok = doc.set_position(0, moved).and_then(|_| doc.to_bytes()).and_then(|bytes| YmapDocument::open(&bytes, None)).map(|back| {
            back.len() == before.len() && back.entity(0).map(|e| e.position) == Some(moved) && (1..before.len()).all(|i| back.entity(i) == before[i])
        });
        if !matches!(ok, Ok(true)) {
            edit_fail += 1;
            failures.push(format!("edit {}: {ok:?}", r.path));
        }
    }
    println!("{files_n} .ymap opened, {with_entities} with entities ({entities} entities)");
    println!("save as is failed: {save_fail}; move one entity, save, reopen and compare failed: {edit_fail}");
    println!("check() said nothing on {quiet} files; findings:");
    for (k, (n, example)) in &issues {
        println!("{n:7}  {k}\n         e.g. {example}");
    }
    for f in failures.iter().take(10) {
        println!("FAILURE {f}");
    }
    Ok(())
}
