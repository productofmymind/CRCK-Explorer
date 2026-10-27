//! Runs only on a machine with GTA V installed (found like `crck-explorer game info` finds it). Reads the user's own game, read-only.
use rage_toolkit_core::{game::Game, model, pipeline};

#[test]
fn retail_archives_open_and_a_vehicle_comes_out_with_textures() {
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    // nested NG-encrypted archive: x64e.rpf -> levels/gta5/vehicles.rpf
    let ar = g.archive("x64e.rpf/levels/gta5/vehicles.rpf").unwrap();
    assert!(ar.contains("adder.yft") && ar.contains("adder.ytd"));
    let out = std::env::temp_dir().join(format!("rage_toolkit_game_{}", std::process::id()));
    let files = g.extract_vehicle("adder", &out).unwrap();
    assert!(files.iter().any(|f| f.ends_with("adder.yft")) && files.iter().any(|f| f.ends_with("vehshare.ytd")));
    let (tex, _) = pipeline::ytd_info(&out.join("adder.ytd")).unwrap();
    assert!(!tex.is_empty() && tex.iter().all(|t| t.width > 0 && t.height > 0));
    let rep = model::inspect(&out.join("adder.yft")).unwrap();
    assert!(rep[0].lods[0].triangles > 10_000);
    std::fs::remove_dir_all(out).unwrap();
}

/// Replace a texture inside a real vehicle YTD from the game with a DDS and with a PNG; the result must parse and keep the others.
#[test]
fn replace_texture_in_a_real_game_ytd() {
    use rage_toolkit_core::texture::{export_texture, replace_texture, ReplaceOpts};
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("rage_toolkit_game_repl_{}", std::process::id()));
    g.extract_vehicle("adder", &out).unwrap();
    let ytd = out.join("adder.ytd");
    let (before, _) = pipeline::ytd_info(&ytd).unwrap();
    let target = before.iter().find(|t| t.name == "adder_badges").expect("adder_badges in adder.ytd");
    // 1) round trip through DDS: export, replace with the exported file, nothing changes in shape
    let dds = out.join("badges.dds");
    export_texture(&ytd, "adder_badges", &dds).unwrap();
    let out1 = out.join("adder_dds.ytd");
    replace_texture(&ytd, "adder_badges", &dds, &out1, ReplaceOpts::default()).unwrap();
    let (a, _) = pipeline::ytd_info(&out1).unwrap();
    assert_eq!(a.len(), before.len());
    let t = a.iter().find(|t| t.name == "adder_badges").unwrap();
    assert_eq!((t.width, t.height, t.format.as_str()), (target.width, target.height, target.format.as_str()));
    // 2) a PNG keeps the DXT5 format of the original
    let png = out.join("new.png");
    image::RgbaImage::from_fn(256, 128, |x, y| image::Rgba([x as u8, y as u8, 200, 255])).save(&png).unwrap();
    let out2 = out.join("adder_png.ytd");
    replace_texture(&ytd, "adder_badges", &png, &out2, ReplaceOpts::default()).unwrap();
    let (b, _) = pipeline::ytd_info(&out2).unwrap();
    assert_eq!(b.iter().find(|t| t.name == "adder_badges").unwrap().format, target.format);
    assert!(b.iter().any(|t| t.name == "adder_badges_n"), "other textures survive");
    std::fs::remove_dir_all(out).unwrap();
}

/// The Adder reads its interior textures from its parent dictionary (vehicles.meta: adder -> vehicles_supergt_interior -> vehshare), and its
/// parts are named after the skeleton's bones.
#[test]
fn adder_gets_its_parent_dictionaries_and_bone_names() {
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("rage_toolkit_game_chain_{}", std::process::id()));
    let files = g.extract_vehicle("adder", &out).unwrap();
    assert!(files.iter().any(|f| f.ends_with("vehicles_supergt_interior.ytd")), "{files:?}");
    let ytds: Vec<_> = files.iter().filter(|f| f.extension().is_some_and(|e| e == "ytd")).cloned().collect();
    let names: Vec<String> = model::load_textures(&out.join("adder.yft"), &ytds).unwrap().into_iter().map(|t| t.name.to_lowercase()).collect();
    for want in ["supergt_interior", "script_rt_dials_supergt", "vehicle_generic_smallspecmap", "adder_badges"] {
        assert!(names.iter().any(|n| n == want), "{want} missing from {} textures", names.len());
    }
    let parts: Vec<String> = model::placed_meshes(&out.join("adder.yft"), model::LodLevel::High).unwrap().into_iter().map(|p| p.part).collect();
    assert!(parts.iter().any(|p| p == "chassis") && parts.iter().any(|p| p == "wheel_lf") && parts.iter().any(|p| p == "wheel_rr"), "{parts:?}");
    std::fs::remove_dir_all(out).unwrap();
}

/// Texture names nothing loaded has are looked up in the game's vehicle dictionaries (the route for modded vehicles, which have no
/// vehicles.meta entry of their own).
#[test]
fn missing_vehicle_textures_are_found_in_their_dictionaries() {
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("rage_toolkit_game_find_{}", std::process::id()));
    let names: Vec<String> = ["supergt_interior", "Vehicle_Generic_SmallSpecMap", "no_such_texture_zz"].map(String::from).to_vec();
    let (files, nobody) = g.find_vehicle_textures(&names, &out).unwrap();
    let leaves: Vec<String> = files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(leaves.contains(&"vehicles_supergt_interior.ytd".to_string()) && leaves.contains(&"vehshare.ytd".to_string()), "{leaves:?}");
    assert_eq!(nobody, vec!["no_such_texture_zz".to_string()]);
    std::fs::remove_dir_all(out).unwrap();
}

/// handling.meta edits on the game's own file: one value changes, one line differs, a copy of an entry can be added and removed again.
#[test]
fn handling_edits_work_on_the_real_vanilla_file() {
    use rage_toolkit_core::handling::{parse, Value};
    use rage_toolkit_core::handling_edit::{duplicate_entry, remove_entry, set_value, FieldRef};
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let file = g.find("handling.meta").into_iter().next().expect("handling.meta in the game");
    let src = String::from_utf8_lossy(&g.read(&file.path).unwrap()).into_owned();
    let p = parse(&src).unwrap();
    let item = p.items.iter().position(|h| h.name.eq_ignore_ascii_case("adder")).expect("adder in the base handling.meta");
    let field = p.items[item].fields.iter().position(|f| f.name == "fMass").unwrap();
    let out = set_value(&src, FieldRef { item, sub: None, field }, &Value::Num(1850.0)).unwrap();
    let changed: Vec<_> = src.lines().zip(out.lines()).filter(|(a, b)| a != b).collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(parse(&out).unwrap().items[item].num("fMass"), Some(1850.0));
    let copy = duplicate_entry(&out, "adder", "zz_test_copy").unwrap();
    assert_eq!(parse(&copy).unwrap().items.len(), p.items.len() + 1);
    let back = remove_entry(&copy, "zz_test_copy").unwrap();
    assert_eq!(parse(&back).unwrap().items.len(), p.items.len());
}

/// Every `.ytyp` of the installed game must read, write and read back to the same tree (this found the float-array writer bug:
/// 92 interior files lost their light data). A sample runs by default (every 25th file, plus an interior with lights and a PSO file);
/// `CRCK_EXPLORER_YTYP_ALL=1` runs all 2600 (about 20 s with `--release`).
#[test]
fn game_ytyp_files_survive_a_rewrite() {
    use rage_toolkit_core::archetype::fidelity;
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let all = std::env::var("CRCK_EXPLORER_YTYP_ALL").is_ok();
    let files = g.find_where(&|p: &str| p.to_lowercase().ends_with(".ytyp"));
    let mut checked = 0;
    for (i, f) in files.iter().enumerate() {
        let pick = all || i % 25 == 0 || f.path.ends_with("v_int_11.ytyp") || f.path.ends_with("des_setpiece.ytyp");
        if !pick {
            continue;
        }
        let data = g.read(&f.path).unwrap();
        let r = fidelity::check(&data).unwrap_or_else(|e| panic!("{}: {e:#}", f.path));
        assert!(r.is_lossless(), "{}: {} differences, first {:?}, warnings {:?} {:?}", f.path, r.total_differences, r.differences.first(), r.read_warnings, r.write_warnings);
        checked += 1;
    }
    assert!(checked > 0 || files.is_empty());
}

/// The metadata validator must not cry wolf: every XML `.meta` / `.xml` of the installed game is well-formed, so none may be reported
/// with a syntax error, and the structure layer should find (almost) nothing in the game's own files. Prints a summary with `--nocapture`.
#[test]
fn game_meta_files_pass_the_metadata_validator() {
    use rage_toolkit_core::metaxml::{self, Severity};
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let files = g.find_where(&|p: &str| {
        let l = p.to_lowercase();
        l.ends_with(".meta") || l.ends_with(".xml")
    });
    let (mut checked, mut checked_structure) = (0, 0);
    let mut syntax: Vec<String> = vec![];
    let mut by_code: std::collections::BTreeMap<(&'static str, String), (usize, String)> = Default::default();
    for f in files.iter().step_by(if std::env::var("CRCK_EXPLORER_META_ALL").is_ok() { 1 } else { 8 }) {
        let Ok(data) = g.read(&f.path) else { continue };
        let Ok(text) = String::from_utf8(data) else { continue };
        if !text.trim_start_matches('\u{feff}').trim_start().starts_with('<') {
            continue;
        }
        let r = metaxml::check(&text);
        checked += 1;
        checked_structure += usize::from(r.structure_checked);
        for d in &r.diagnostics {
            let e = by_code.entry((d.code, format!("{:?}", d.severity))).or_insert((0, String::new()));
            e.0 += 1;
            if e.1.is_empty() {
                e.1 = format!("{} line {}: {}", f.path, d.line, d.message);
            }
            if d.severity == Severity::Error {
                syntax.push(format!("{}: {} line {} col {}: {}", f.path, d.code, d.line, d.col, d.message));
            }
        }
    }
    println!("{checked} game XML files checked, {checked_structure} with a known structure");
    for ((code, sev), (n, example)) in &by_code {
        println!("  {code} [{sev}]: {n}   e.g. {example}");
    }
    for s in syntax.iter().take(15) {
        println!("  SYNTAX {s}");
    }
    assert!(syntax.is_empty(), "{} errors on the game's own files (listed above)", syntax.len());
}

/// A vehicle's geometries are named after the bone their vertices follow (bodyshell, windscreen, ...), not all "chassis".
#[test]
fn vehicle_parts_are_named_after_their_bones() {
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("rage_toolkit_parts_{}", std::process::id()));
    g.extract_vehicle("adder", &out).unwrap();
    let parts = model::placed_meshes(&out.join("adder.yft"), rage_formats::ydd::LodLevel::High).unwrap();
    let names: Vec<&str> = parts.iter().map(|p| p.part.as_str()).collect();
    for expect in ["bodyshell", "windscreen", "chassis", "wheel_lf"] {
        assert!(names.contains(&expect), "{expect} missing from {names:?}");
    }
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "every part has its own name: {names:?}");
    std::fs::remove_dir_all(out).unwrap();
}

/// The clothing YMT generator against the game's own freemode YMTs: for each one, take its shape (how many drawables per
/// component, textures per drawable, props per anchor), generate a YMT from that shape alone, and compare the parts that follow from
/// the shape: `availComp`, each component's `numAvailTex`, every drawable's texture count and cloth flag, the `compInfos` indices,
/// the props' anchor / id / texture count and the anchor lists. A sample by default; `CRCK_EXPLORER_YMT_ALL=1` runs all of them.
#[test]
fn generated_ymts_have_the_structure_of_the_games_own() {
    use rage_toolkit_core::clothing::build::{ymt_bytes, Shape};
    use rage_formats::{dump_meta, MetaValue};
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    // the facts that follow from the shape, as comparable text
    fn facts(root: &MetaValue) -> Vec<String> {
        let mut out = vec![];
        let r = root.as_struct().unwrap();
        out.push(format!("availComp {:?}", r.field("availComp")));
        // (a few of the game's files carry a component entry that availComp does not point to; it is not part of the shape)
        let referenced: Vec<usize> = match r.field("availComp") {
            Some(MetaValue::Bytes(b)) => b.iter().filter(|x| **x != 255).map(|x| *x as usize).collect(),
            _ => vec![],
        };
        for (pos, c) in r.field("aComponentData3").map(|v| v.items()).unwrap_or(&[]).iter().enumerate() {
            if !referenced.contains(&pos) {
                continue;
            }
            let c = c.as_struct().unwrap();
            out.push(format!("numAvailTex {:?}", c.field("numAvailTex").and_then(|v| v.as_i64())));
            for d in c.field("aDrawblData3").map(|v| v.items()).unwrap_or(&[]) {
                let d = d.as_struct().unwrap();
                // (numAlternatives, the first-person alternates, is not modelled)
                out.push(format!("drawable tex {} cloth {:?}", d.field("aTexData").map(|t| t.items().len()).unwrap_or(0), d.field("clothData").and_then(|c| c.as_struct()).and_then(|c| c.field("ownsCloth"))));
            }
        }
        // the game lists compInfos in the order the clothes were authored, not sorted; each names its own component and index,
        // so they are compared as a set
        let mut infos: Vec<(i64, i64)> = r.field("compInfos").map(|v| v.items()).unwrap_or(&[]).iter().map(|i| i.as_struct().unwrap()).map(|i| (i.field("pedXml_compIdx").and_then(|v| v.as_i64()).unwrap_or(-1), i.field("pedXml_drawblIdx").and_then(|v| v.as_i64()).unwrap_or(-1))).collect();
        infos.sort();
        out.extend(infos.iter().map(|(c, d)| format!("compInfo {c} {d}")));
        if let Some(p) = r.field("propInfo").and_then(|p| p.as_struct()) {
            out.push(format!("numAvailProps {:?}", p.field("numAvailProps").and_then(|v| v.as_i64())));
            for m in p.field("aPropMetaData").map(|v| v.items()).unwrap_or(&[]) {
                let m = m.as_struct().unwrap();
                out.push(format!("prop anchor {:?} id {:?} tex {}", m.field("anchorId").and_then(|v| v.as_i64()), m.field("propId").and_then(|v| v.as_i64()), m.field("texData").map(|t| t.items().len()).unwrap_or(0)));
            }
            for a in p.field("aAnchors").map(|v| v.items()).unwrap_or(&[]) {
                let a = a.as_struct().unwrap();
                out.push(format!("anchor {:?}", a.field("props")));
            }
        }
        out
    }
    let all = std::env::var("CRCK_EXPLORER_YMT_ALL").is_ok();
    let files = g.find_where(&|p: &str| {
        let l = p.to_lowercase();
        (l.contains("mp_m_freemode_01_") || l.contains("mp_f_freemode_01_")) && l.ends_with(".ymt") && !l.contains("creaturemetadata")
    });
    let (mut checked, mut drawables) = (0, 0);
    for f in files.iter().step_by(if all { 1 } else { 9 }) {
        let data = g.read(&f.path).unwrap();
        let Ok(vanilla) = dump_meta(&data) else { continue };
        if vanilla.root.as_struct().and_then(|s| s.field("availComp")).is_none() {
            continue;
        }
        let shape = Shape::of_tree(&vanilla.root);
        let ours = dump_meta(&ymt_bytes(&shape, "x").unwrap_or_else(|e| panic!("{}: {e:#}", f.path))).unwrap();
        let (a, b) = (facts(&vanilla.root), facts(&ours.root));
        if let Some(i) = a.iter().zip(&b).position(|(x, y)| x != y).or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len()))) {
            panic!("{}: differs at fact {i}: game {:?} vs ours {:?}", f.path, a.get(i), b.get(i));
        }
        drawables += shape.components.values().map(Vec::len).sum::<usize>() + shape.props.values().map(Vec::len).sum::<usize>();
        checked += 1;
    }
    println!("{checked} game YMTs reproduced from their shape ({drawables} drawables and props)");
    assert!(checked > 0 || files.is_empty());
}

/// The archetype document on the game's own files: every `.ytyp` opens as a document, an edit and its undo leave it as it was, and it
/// saves (the save check reads the bytes back and refuses any loss) into a file that opens with the same archetypes. A sample by
/// default; `CRCK_EXPLORER_YTYP_ALL=1` runs all of them.
#[test]
fn game_ytyp_files_open_edit_undo_and_save_as_documents() {
    use rage_toolkit_core::archetype::document::{ArchKind, YtypDocument};
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let all = std::env::var("CRCK_EXPLORER_YTYP_ALL").is_ok();
    let files = g.find_where(&|p: &str| p.to_lowercase().ends_with(".ytyp"));
    let mut checked = 0;
    for (i, f) in files.iter().enumerate() {
        if !(all || i % 40 == 0 || f.path.ends_with("v_int_11.ytyp") || f.path.ends_with("des_setpiece.ytyp")) {
            continue;
        }
        let data = g.read(&f.path).unwrap();
        let mut d = YtypDocument::open(&data, None).unwrap_or_else(|e| panic!("{}: {e:#}", f.path));
        let n = d.len();
        let before = d.to_xml(None);
        if n > 0 {
            d.rename_archetype(0, "rage_toolkit_test_name").unwrap();
            d.add_archetype(ArchKind::Base, "rage_toolkit_added").unwrap();
            assert_eq!(d.len(), n + 1);
            while d.can_undo() {
                d.undo().unwrap();
            }
            assert_eq!(d.to_xml(None), before, "{}: undo restores the file", f.path);
        }
        let bytes = d.to_bytes().unwrap_or_else(|e| panic!("{}: {e:#}", f.path));
        assert_eq!(YtypDocument::open(&bytes, None).unwrap().len(), n, "{}", f.path);
        checked += 1;
    }
    assert!(checked > 0 || files.is_empty());
}

/// The archetype checks must say nothing (errors) about the game's own `.ytyp` files; the counts per code are printed with `--nocapture`
/// (how the thresholds were calibrated). With the game's models and texture dictionaries indexed, missing references are measured too.
#[test]
fn archetype_checks_do_not_cry_wolf_on_the_games_own_files() {
    use rage_toolkit_core::archetype::{
        document::YtypDocument,
        validate::{validate, AssetIndex, Sev},
    };
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let ix = AssetIndex::from_game(&g);
    let all = std::env::var("CRCK_EXPLORER_YTYP_ALL").is_ok();
    let files = g.find_where(&|p: &str| p.to_lowercase().ends_with(".ytyp"));
    let mut by: std::collections::BTreeMap<(&'static str, String), (usize, String)> = Default::default();
    let (mut checked, mut archetypes) = (0, 0);
    for (i, f) in files.iter().enumerate() {
        if !(all || i % 10 == 0) {
            continue;
        }
        let d = YtypDocument::open(&g.read(&f.path).unwrap(), None).unwrap();
        archetypes += d.len();
        for x in validate(&d, Some(&ix)) {
            let e = by.entry((x.code, format!("{:?}", x.sev))).or_insert((0, String::new()));
            e.0 += 1;
            if e.1.is_empty() {
                e.1 = format!("{}: {}", f.path, x.message);
            }
        }
        checked += 1;
    }
    println!("{checked} game ytyp files, {archetypes} archetypes, {} indexed asset files", ix.files);
    for ((code, sev), (n, ex)) in &by {
        println!("  {code} [{sev}]: {n}   e.g. {ex}");
    }
    let _ = Sev::Error;
    assert!(!by.keys().any(|(_, s)| s == "Error"), "errors on the game's own files");
}

/// Merging real archetype files: a file merged into itself is all identical, two different files merge by adding the new ones
/// (an MLO file included), the merge is one undo step, and the result saves.
#[test]
fn game_ytyp_files_merge() {
    use rage_toolkit_core::archetype::{
        document::YtypDocument,
        merge::{apply, default_choices, plan, Status},
    };
    let Ok(g) = Game::open(None) else {
        eprintln!("no GTA V install found: skipped");
        return;
    };
    let open = |p: &str| YtypDocument::open(&g.read(p).unwrap(), None).unwrap();
    let (a_path, b_path, mlo_path) = ("x64c.rpf/levels/gta5/_prologue/plg_01.rpf/plg_01.ytyp", "x64c.rpf/levels/gta5/_prologue/plg_02.rpf/plg_02.ytyp", "x64h.rpf/levels/gta5/interiors/v_int_11.rpf/v_int_11.ytyp");
    if g.read(a_path).is_err() {
        return;
    }
    let mut a = open(a_path);
    let a2 = open(a_path);
    let same = plan(&a, &a2);
    assert!(same.entries.iter().all(|e| e.status == Status::Identical), "a file merged into itself is all identical");
    let before = a.len();
    let b = open(b_path);
    let p = plan(&a, &b);
    assert!(p.count(Status::New) > 0);
    let r = apply(&mut a, &b, &p, &default_choices(&p), true).unwrap();
    assert_eq!(a.len(), before + r.added);
    let bytes = a.to_bytes().unwrap();
    assert_eq!(YtypDocument::open(&bytes, None).unwrap().len(), a.len());
    a.undo().unwrap();
    assert_eq!(a.len(), before, "one undo takes the merge back");
    // an interior file: its MLO comes along with its rooms
    let mlo = open(mlo_path);
    let p = plan(&a, &mlo);
    apply(&mut a, &mlo, &p, &default_choices(&p), true).unwrap();
    a.to_bytes().unwrap();
}
