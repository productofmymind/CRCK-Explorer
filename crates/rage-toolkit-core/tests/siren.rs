use rage_toolkit_core::siren;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/siren").join(name)
}

fn codes(r: &rage_toolkit_core::validate::Report, sev: char) -> Vec<&'static str> {
    r.issues.iter().filter(|i| i.sev == sev).map(|i| i.code).collect()
}

#[test]
fn good_pack_has_no_errors_or_warnings() {
    let r = siren::check(&fixture("good")).unwrap();
    assert_eq!((r.errors, codes(&r, 'W')), (0, vec![]), "{}", r.summary);
    // the three informational cases: steady lights, emissive-only rotators, a vanilla id used by the pack
    let info = codes(&r, 'I');
    for c in ["siren.steady", "siren.dark", "siren.var-external"] {
        assert!(info.contains(&c), "{c} missing from {info:?}");
    }
    assert!(r.summary.starts_with("2 siren setting(s) in 1 carcols.meta, 1 carvariations.meta"), "{}", r.summary);
}

#[test]
fn a_single_carcols_checks_its_neighbour_carvariations_too() {
    let r = siren::check(&fixture("good/carcols.meta")).unwrap();
    assert!(r.summary.contains("1 carvariations.meta"), "{}", r.summary);
}

#[test]
fn bad_pack_trips_every_rule() {
    let r = siren::check(&fixture("bad")).unwrap();
    assert!(r.errors > 0);
    let (e, w, i) = (codes(&r, 'E'), codes(&r, 'W'), codes(&r, 'I'));
    for c in ["siren.id", "siren.id-dup", "siren.sequencer", "siren.number", "siren.var-bad"] {
        assert!(e.contains(&c), "{c} missing from errors {e:?}");
    }
    for c in ["siren.radians", "siren.color", "siren.corona", "siren.count", "siren.id-vanilla", "siren.id-range", "siren.bpm"] {
        assert!(w.contains(&c), "{c} missing from warnings {w:?}");
    }
    for c in ["siren.count", "siren.unused", "siren.var-external"] {
        assert!(i.contains(&c), "{c} missing from info {i:?}");
    }
    // every issue names a file and carries a fix hint
    assert!(r.issues.iter().all(|g| !g.file.is_empty() && !g.fix.is_empty()));
}

#[test]
fn missing_path_is_an_error() {
    assert!(siren::check(&fixture("nope")).is_err());
    assert!(siren::check(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")).is_err(), "a folder without carcols.meta / carvariations.meta");
}
