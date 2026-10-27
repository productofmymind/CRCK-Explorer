use rage_toolkit_core::validate::{run, Profile, Report};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/validate").join(name)
}

fn codes(r: &Report) -> Vec<&'static str> {
    r.issues.iter().map(|i| i.code).filter(|c| !c.is_empty()).collect()
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rage_toolkit_test_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("stream")).unwrap();
    d
}

#[test]
fn good_fivem_has_no_errors() {
    let r = run(&fixture("good_fivem"), Profile::Fivem);
    assert_eq!(r.errors, 0, "{:?}", codes(&r));
    assert!(codes(&r).is_empty(), "{:?}", codes(&r));
}

#[test]
fn bad_fivem_reports_each_rule() {
    let r = run(&fixture("bad_fivem"), Profile::Fivem);
    let c = codes(&r);
    for want in [
        "MANIFEST_FX_VERSION",
        "MANIFEST_GAME",
        "MANIFEST_META_NOT_IN_FILES",
        "MANIFEST_NO_DATA_FILE",
        "MANIFEST_DATA_FILE_MISSING",
        "STRUCTURE_STREAM_IN_ROOT",
        "STREAM_NO_YTD",
        "STREAM_HI_NO_BASE",
        "STRUCTURE_JUNK_FILES",
    ] {
        assert!(c.contains(&want), "missing {want}; got {c:?}");
    }
    assert!(r.errors > 0);
}

#[test]
fn core_profile_ignores_fivem_rules() {
    let r = run(&fixture("bad_fivem"), Profile::Core);
    assert!(codes(&r).is_empty());
}

#[test]
fn ytd_size_limits() {
    for (mb, want) in [(17u64, Some("YTD_OVER_LIMIT")), (13, Some("YTD_NEAR_LIMIT")), (4, None)] {
        let d = scratch(&format!("ytd{mb}"));
        fs::write(d.join("fxmanifest.lua"), "fx_version 'cerulean'\ngame 'gta5'\n").unwrap();
        fs::write(d.join("stream/a.yft"), b"x").unwrap();
        fs::File::create(d.join("stream/a.ytd")).unwrap().set_len(mb * 1024 * 1024).unwrap();
        let c = codes(&run(&d, Profile::Fivem));
        match want {
            Some(w) => assert!(c.contains(&w), "{mb} MB: {c:?}"),
            None => assert!(!c.iter().any(|x| x.starts_with("YTD_")), "{mb} MB: {c:?}"),
        }
        fs::remove_dir_all(d).unwrap();
    }
}

#[test]
fn manifest_cases() {
    // no manifest at all
    let d = scratch("nomanifest");
    assert!(codes(&run(&d, Profile::Fivem)).contains(&"MANIFEST_MISSING"));
    // legacy
    fs::write(d.join("__resource.lua"), "resource_manifest_version '44febabe-d386-4d18-afbe-5e627f4af937'\n").unwrap();
    assert!(codes(&run(&d, Profile::Fivem)).contains(&"MANIFEST_LEGACY"));
    // nested
    fs::remove_file(d.join("__resource.lua")).unwrap();
    fs::create_dir_all(d.join("sub")).unwrap();
    fs::write(d.join("sub/fxmanifest.lua"), "fx_version 'cerulean'\n").unwrap();
    assert!(codes(&run(&d, Profile::Fivem)).contains(&"STRUCTURE_NESTED_RESOURCE"));
    fs::remove_dir_all(d).unwrap();
}
