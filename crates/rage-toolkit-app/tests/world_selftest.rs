//! The World editor driven the way a person drives it: the app starts with a window, loads the whole game, and a script feeds it mouse and
//! key events (click, Ctrl-click, box select, drag the gizmo, Shift-drag copy, grid and ground snap, delete, copy, paste, undo, choose a LOD
//! parent, open a real game map for editing and save it) and checks what happened after each step. See `src/world/script.rs`.
//!
//! Ignored by default: it needs a GTA V install and a window with a GPU, and takes some seconds once the game's index is cached.
//! `cargo test --release -p rage-toolkit-app --test world_selftest -- --ignored --nocapture`
use std::process::Command;

#[test]
#[ignore = "needs a GTA V install and a window: cargo test --release -p rage-toolkit-app --test world_selftest -- --ignored"]
fn the_world_editor_passes_its_scripted_self_test() {
    let out = Command::new(env!("CARGO_BIN_EXE_crck-explorer")).args(["--world-view", "195,-960,60", "--world-selftest"]).output().expect("starting the app");
    let text = String::from_utf8_lossy(&out.stderr);
    println!("{text}");
    assert!(text.contains(" checks, "), "the self-test did not finish (is the game installed?):\n{text}");
    assert!(text.contains(", 0 failed"), "some checks failed:\n{text}");
}
