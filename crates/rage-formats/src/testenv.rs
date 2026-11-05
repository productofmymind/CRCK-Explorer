//! Tests that need real game files read their location from an environment variable. Without it they used to return and
//! pass; now they say so, and `CRCK_REQUIRE_REAL_FILES=1` turns a missing variable into a failure.
use std::ffi::OsString;

fn missing(name: &str) {
    eprintln!("SKIPPED: set {name} to run this test on real game files");
    if std::env::var_os("CRCK_REQUIRE_REAL_FILES").is_some() {
        panic!("{name} is not set and CRCK_REQUIRE_REAL_FILES is");
    }
}

pub fn var_os(name: &str) -> Option<OsString> {
    let v = std::env::var_os(name);
    if v.is_none() {
        missing(name);
    }
    v
}

pub fn var(name: &str) -> Option<String> {
    var_os(name).and_then(|v| v.into_string().ok())
}
