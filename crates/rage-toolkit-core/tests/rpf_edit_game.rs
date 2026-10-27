//! Editing copies of the installed game's archives (in a temporary folder; the game is only read). Skips itself when GTA V is
//! not found. Each test reads every file before and after: the edited archive is OPEN (unencrypted) afterwards and every
//! file must read back the same, plus the edit.
use rage_toolkit_core::{
    game::{Archive, Game},
    rpf_edit::{edit, Edit},
};
use std::{collections::BTreeMap, fs, path::PathBuf};

fn game() -> Option<Game> {
    let g = Game::open(None).ok();
    if g.is_none() {
        eprintln!("skipped: GTA V not found");
    }
    g
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rage_toolkit_rpfedit_game_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn contents(ar: &Archive) -> BTreeMap<String, Vec<u8>> {
    ar.files().into_iter().map(|f| (f.path.to_lowercase(), ar.read(&f.path).unwrap_or_else(|e| panic!("{}: {e:#}", f.path)))).collect()
}

#[test]
fn an_encrypted_game_archive_becomes_open_with_every_file_intact() {
    let Some(g) = game() else { return };
    let d = temp("common");
    let src = g.dir.join("common.rpf");
    let copy = d.join("common.rpf");
    fs::copy(&src, &copy).unwrap();
    let keys = g.keys();
    let before = contents(&Archive::open(&src, Some(keys.clone())).unwrap());
    let r = edit(&copy, &[], &[Edit::Put { path: "data/rage_toolkit_test.txt".into(), data: b"hello".to_vec() }], Some(&keys), None).unwrap();
    assert_eq!(r.converted, ["common.rpf"]);
    let mut after = contents(&Archive::open(&copy, None).expect("OPEN now: no keys needed"));
    assert_eq!(after.remove("data/rage_toolkit_test.txt").as_deref(), Some(&b"hello"[..]));
    assert_eq!(after.len(), before.len());
    for (p, v) in &before {
        assert!(after.get(p) == Some(v), "{p} differs after the conversion");
    }
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn an_edit_inside_a_nested_game_archive_keeps_both_levels_intact() {
    let Some(g) = game() else { return };
    let keys = g.keys();
    // the smallest DLC pack that has an archive inside
    let mut packs: Vec<(u64, String, String)> = g
        .top_level_archives()
        .into_iter()
        .filter(|t| t.ends_with("/dlc.rpf"))
        .filter_map(|t| {
            let size = fs::metadata(g.dir.join(&t)).ok()?.len();
            let ar = Archive::open(&g.dir.join(&t), Some(keys.clone())).ok()?;
            let nested = ar.files().into_iter().find(|f| f.path.to_lowercase().ends_with(".rpf"))?.path;
            Some((size, t, nested))
        })
        .collect();
    packs.sort();
    let Some((_, pack, nested)) = packs.into_iter().next() else { return };
    let d = temp("nested");
    let src = g.dir.join(&pack);
    let copy = d.join("dlc.rpf");
    fs::copy(&src, &copy).unwrap();
    let orig = Archive::open(&src, Some(keys.clone())).unwrap();
    let (outer_before, inner_before) = (contents(&orig), contents(&orig.child(&nested).unwrap()));
    let r = edit(&copy, std::slice::from_ref(&nested), &[Edit::Put { path: "crck-explorer/added.meta".into(), data: b"<added/>".to_vec() }], Some(&keys), None).unwrap();
    assert_eq!(r.converted.len(), 2, "{pack}: both archives on the way are OPEN now ({:?})", r.converted);
    let ar = Archive::open(&copy, None).unwrap();
    let mut inner_after = contents(&ar.child(&nested).unwrap());
    assert_eq!(inner_after.remove("crck-explorer/added.meta").as_deref(), Some(&b"<added/>"[..]));
    assert_eq!(inner_after, inner_before, "{pack} > {nested}");
    let outer_after = contents(&ar);
    for (p, v) in &outer_before {
        if p != &nested.to_lowercase() {
            assert!(outer_after.get(p) == Some(v), "{p} differs");
        }
    }
    fs::remove_dir_all(d).unwrap();
}
