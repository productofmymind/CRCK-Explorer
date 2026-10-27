use rage_toolkit_core::handling::{self, CompareOptions};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/handling").join(name)
}

fn opts(paths: Vec<PathBuf>) -> CompareOptions {
    CompareOptions { paths, game: false, game_dir: None, fields: vec![], sort: None, desc: false, filter: vec![], limit: None }
}

#[test]
fn good_fixture_has_no_issues_at_all() {
    let r = handling::check(&[fixture("good")]).unwrap();
    assert!(r.issues.is_empty(), "{:?}", r.issues.iter().map(|i| &i.msg).collect::<Vec<_>>());
}

#[test]
fn bad_fixture_reports_each_problem_once() {
    let r = handling::check(&[fixture("bad")]).unwrap();
    let mut got: Vec<(char, &str)> = r.issues.iter().map(|i| (i.sev, i.code)).collect();
    got.sort();
    let mut want = vec![
        ('E', "HANDLING_BAD_VALUE"),        // fSteeringLock "forty"
        ('E', "HANDLING_MISSING_FIELD"),    // BADCAR has no fBrakeForce
        ('E', "HANDLING_BAD_FLAGS"),        // 44X010
        ('E', "HANDLING_DUPLICATE_NAME"),   // badcar vs BADCAR
        ('W', "HANDLING_OUT_OF_RANGE"),     // fMass 250000
        ('W', "HANDLING_OUT_OF_RANGE"),     // fTractionCurveMax 9
        ('W', "HANDLING_BAD_VALUE"),        // 6.5 gears
        ('W', "HANDLING_DUPLICATE_FIELD"),  // fMass twice in badcar
        ('W', "HANDLING_MISSING_FIELD"),    // badcar has no fOilVolume
        ('W', "HANDLING_ITEM_TYPE"),        // CHandlingDat
        ('I', "HANDLING_UNKNOWN_FIELD"),    // fMadeUp
    ];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(r.errors, 4);
}

#[test]
fn compare_fixture_lists_addon_next_to_other_files_sorted() {
    let mut o = opts(vec![fixture("compare/addon.meta"), fixture("good")]);
    o.sort = Some("top-speed".into());
    o.desc = true;
    let t = handling::compare(&o).unwrap();
    let names: Vec<&str> = t.rows.iter().map(|r| r[0].text.as_str()).collect();
    assert_eq!(names, ["FIXPLANE", "ADDONCAR", "FIXCAR", "FIXQUAD", "FIXBIKE", "ADDONTRUCK"]);
    assert!(t.csv().contains("ADDONCAR,car,1720,0.34,172,1.1,2.6,0.4,7,38,addon.meta"));
}

/// Needs a GTA V install (skipped otherwise) and about a minute: `cargo test -p rage-toolkit-core --test handling -- --ignored`
#[test]
#[ignore]
fn every_vanilla_handling_file_is_silent_and_inside_the_bands() {
    let Ok(src) = handling::game_sources(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    for s in &src {
        let r = handling::check_sources(std::slice::from_ref(s));
        assert!(r.issues.is_empty(), "{}: {:?}", s.label, r.issues.iter().map(|i| &i.msg).collect::<Vec<_>>());
    }
    let rows = handling::ranges(&handling::load(&[], true, None).unwrap());
    assert!(!rows.is_empty() && rows.iter().all(|r| r.outside_band == 0));
}
