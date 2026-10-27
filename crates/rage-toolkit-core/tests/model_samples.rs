//! Runs only when CRCK_EXPLORER_SAMPLES points at a folder with real files (e.g. a FiveM tex_overrides clothing folder
//! containing lowr_031_u.ydd and lowr_diff_031_c_uni.ytd). Real game assets are never committed, so CI skips this.
use rage_toolkit_core::model;
use std::{fs, path::PathBuf};

#[test]
fn real_clothing_model_with_gen9_texture() {
    let Some(dir) = std::env::var_os("CRCK_EXPLORER_SAMPLES").map(PathBuf::from) else {
        eprintln!("CRCK_EXPLORER_SAMPLES not set: skipped");
        return;
    };
    let (ydd, ytd) = (dir.join("lowr_031_u.ydd"), dir.join("lowr_diff_031_c_uni.ytd"));
    if !ydd.exists() || !ytd.exists() {
        eprintln!("sample files not found in {}: skipped", dir.display());
        return;
    }
    let rep = model::inspect(&ydd).unwrap();
    assert_eq!(rep[0].lods[0].triangles, 3310);
    assert_eq!(rep[0].lods[0].vertices, 1944);
    assert!(model::missing_textures(&rep, &model::ytd_names(&ytd).unwrap()) == vec!["givemechecker".to_string()]);
    let out = std::env::temp_dir().join(format!("rage_toolkit_obj_{}", std::process::id()));
    model::export_obj(&ydd, &out, Some(&ytd), model::LodLevel::High).unwrap();
    let obj = fs::read_to_string(out.join("lowr_031_u.obj")).unwrap();
    assert_eq!(obj.lines().filter(|l| l.starts_with("f ")).count(), 3310);
    assert!(fs::metadata(out.join("textures/lowr_diff_000_a_uni.png")).unwrap().len() > 10_000);
    fs::remove_dir_all(out).unwrap();
}

/// Vehicle fragments: CRCK_EXPLORER_VEHICLE_SAMPLES = folder containing volatus.yft (a base-game helicopter).
#[test]
fn real_vehicle_fragment_is_assembled() {
    let Some(dir) = std::env::var_os("CRCK_EXPLORER_VEHICLE_SAMPLES").map(PathBuf::from) else {
        eprintln!("CRCK_EXPLORER_VEHICLE_SAMPLES not set: skipped");
        return;
    };
    let yft = dir.join("volatus.yft");
    if !yft.exists() {
        eprintln!("{} not found: skipped", yft.display());
        return;
    }
    let rep = model::inspect(&yft).unwrap();
    assert_eq!(rep[0].lods[0].triangles, 17041);
    let parts = model::placed_meshes(&yft, model::LodLevel::High).unwrap();
    // the main drawable plus the physics children (doors, rotor parts...) placed on it: 19219 on the sample
    assert!(parts.iter().map(|p| p.indices.len() / 3).sum::<usize>() >= 17041);
    // fuselage + rotor + tail all in place: the assembled model spans roughly the 12 x 15 m the file declares
    let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in parts.iter().flat_map(|p| p.positions.iter()) {
        for i in 0..3 {
            mn[i] = mn[i].min(p[i]);
            mx[i] = mx[i].max(p[i]);
        }
    }
    let ext = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
    assert!(ext[0] > 8.0 && ext[1] > 10.0 && ext[2] > 3.0 && ext[0] < 20.0 && ext[1] < 20.0 && ext[2] < 8.0, "{ext:?}");
}
