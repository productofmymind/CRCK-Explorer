//! The bare freemode body the clothing preview dresses: head, hair, bare torso, legs and feet of `mp_m_freemode_01` /
//! `mp_f_freemode_01`, read from the user's own game (`streamedpeds_mp.rpf`) once and kept in the cache folder. The game is
//! only read.
use super::Sex;
use crate::game::{cache_dir, Game};
use anyhow::{Context, Result};
use std::{fs, path::PathBuf};

/// where the base freemode peds are in the game
const BASE: &str = "x64v.rpf/models/cdimages/streamedpeds_mp.rpf";

/// The body parts shown under the clothes: component index, its name, the drawable number (hair 000: hair cards of
/// longer styles cover the face up close; torso 015 is bare; feet 005 are the flip-flops).
pub const PARTS: [(u8, &str, u32); 5] = [(0, "head", 0), (2, "hair", 0), (3, "uppr", 15), (4, "lowr", 15), (6, "feet", 5)];

/// One body part on disk: which component it is, its model and its first texture.
#[derive(Debug, Clone)]
pub struct Part {
    pub component: u8,
    pub ydd: PathBuf,
    pub ytd: Option<PathBuf>,
}

/// The body parts for `sex`, copied out of the game into the cache the first time.
pub fn freemode_body(game: &Game, sex: Sex) -> Result<Vec<Part>> {
    let dir = cache_dir().join("body").join(sex.ped());
    fs::create_dir_all(&dir)?;
    let mut archive = None;
    let mut out = vec![];
    for (component, name, number) in PARTS {
        let (model, texture) = (format!("{name}_{number:03}_"), format!("{name}_diff_{number:03}_a_"));
        // already there from an earlier run
        let have = |prefix: &str, ext: &str| fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix) && n.to_string_lossy().ends_with(ext)));
        if let (Some(ydd), Some(ytd)) = (have(&model, ".ydd"), have(&texture, ".ytd")) {
            out.push(Part { component, ydd, ytd: Some(ytd) });
            continue;
        }
        if archive.is_none() {
            archive = Some(game.archive(BASE).with_context(|| format!("{BASE} (the base peds)"))?);
        }
        let ar = archive.as_ref().unwrap();
        let folder = format!("{}/", sex.ped());
        // the plain model (`hair_000_u.ydd`, not the `_1` alternate) and its first texture
        let pick = |prefix: &str, ext: &str| {
            let mut v: Vec<String> = ar.files().into_iter().map(|f| f.path).filter(|p| p.strip_prefix(&folder).is_some_and(|n| n.starts_with(prefix) && n.ends_with(ext))).collect();
            v.sort_by_key(|p| p.len());
            v.into_iter().next()
        };
        let Some(ydd_in) = pick(&model, ".ydd") else { continue };
        let save = |inner: &str| -> Result<PathBuf> {
            let p = dir.join(inner.rsplit('/').next().unwrap_or(inner));
            fs::write(&p, ar.read(inner)?)?;
            Ok(p)
        };
        let ydd = save(&ydd_in)?;
        let ytd = pick(&texture, ".ytd").map(|t| save(&t)).transpose()?;
        out.push(Part { component, ydd, ytd });
    }
    Ok(out)
}

/// The freemode ped's skeleton for `sex` (its `.yft` from the game, kept in the cache): the bones with their bind pose.
pub fn freemode_skeleton(game: &Game, sex: Sex) -> Result<Vec<crate::model::Bone>> {
    let dir = cache_dir().join("body").join(sex.ped());
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.yft", sex.ped()));
    if !file.is_file() {
        let ar = game.archive(BASE).with_context(|| format!("{BASE} (the base peds)"))?;
        fs::write(&file, ar.read(&format!("{}.yft", sex.ped()))?)?;
    }
    let bones = crate::model::fragment_bones(&fs::read(&file)?);
    if bones.is_empty() {
        anyhow::bail!("{}: no skeleton", file.display());
    }
    Ok(bones)
}

/// The bone a prop anchor hangs on, by the bone's tag (hats, glasses, earrings and masks on the head; watches and bracelets on
/// the forearm roll bones, which sit at the wrist; the hand props on the hands' prop helpers; the rest where they are named).
/// Checked by placing GTA World's hats, glasses, earrings and watches on the freemode skeleton.
pub fn anchor_bone_tag(anchor: u8) -> u16 {
    match anchor {
        0..=3 => 12844, // IK_Head: head, eyes, ears, mouth
        4 => 60309,     // PH_L_Hand
        5 => 28422,     // PH_R_Hand
        6 => 61007,     // RB_L_ForeArmRoll (left wrist)
        7 => 43810,     // RB_R_ForeArmRoll (right wrist)
        8 => 11816,     // SKEL_Pelvis (hip)
        9 => 14201,     // SKEL_L_Foot
        10 => 52301,    // SKEL_R_Foot
        11 => 60309,    // PH_L_Hand
        _ => 28422,     // PH_R_Hand
    }
}

/// Where a prop of `anchor` sits on the ped: the anchor bone's bind pose in the ped's space, or `None` when the skeleton has
/// no such bone.
pub fn anchor_matrix(bones: &[crate::model::Bone], anchor: u8) -> Option<glam::Mat4> {
    let world = crate::model::bone_world(bones);
    let tag = anchor_bone_tag(anchor);
    bones.iter().position(|b| b.tag == tag).map(|i| world[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the skeleton's bones as text (for checking anchor bones): `cargo test -p rage-toolkit-core skeleton_bones -- --ignored --nocapture`
    #[test]
    #[ignore = "prints the freemode skeleton; needs the game"]
    fn skeleton_bones() {
        let g = Game::open(None).unwrap();
        let bones = freemode_skeleton(&g, Sex::Male).unwrap();
        let world = crate::model::bone_world(&bones);
        for (b, w) in bones.iter().zip(&world) {
            let t = w.w_axis;
            println!("{:3} {:5} {:28} parent {:?} at ({:.3}, {:.3}, {:.3})", b.index, b.tag, b.name, b.parent, t.x, t.y, t.z);
        }
    }

    /// Props of a real pack placed on their bone (CRCK_PROP_FILES = `;`-separated `<anchor>=<path>`): their bounds before and
    /// after, next to the head's.
    #[test]
    #[ignore = "needs the game and props: CRCK_PROP_FILES=0=C:\\..\\p_head_000.ydd;1=..."]
    fn real_props_sit_on_their_bone() {
        let g = Game::open(None).unwrap();
        let bones = freemode_skeleton(&g, Sex::Male).unwrap();
        let bounds = |ms: &[crate::model::MeshPart]| {
            let (mut a, mut b) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
            for p in ms.iter().flat_map(|m| &m.positions) {
                a = a.min(glam::Vec3::from(*p));
                b = b.max(glam::Vec3::from(*p));
            }
            (a, b)
        };
        let head = freemode_body(&g, Sex::Male).unwrap().into_iter().find(|p| p.component == 0).unwrap();
        println!("head mesh: {:?}", bounds(&crate::model::placed_meshes(&head.ydd, crate::model::LodLevel::High).unwrap()));
        for item in std::env::var("CRCK_PROP_FILES").unwrap().split(';') {
            let (a, path) = item.split_once('=').unwrap();
            let mut ms = crate::model::placed_meshes(std::path::Path::new(path), crate::model::LodLevel::High).unwrap();
            let before = bounds(&ms);
            crate::model::transform_meshes(&mut ms, anchor_matrix(&bones, a.parse().unwrap()).unwrap());
            println!("anchor {a} {path}\n  as stored {before:?}\n  on the bone {:?}", bounds(&ms));
        }
        // other bones to try for a prop (CRCK_TRY = `<tag>,<tag>=<path>`)
        if let Ok(t) = std::env::var("CRCK_TRY") {
            let (tags, path) = t.split_once('=').unwrap();
            let world = crate::model::bone_world(&bones);
            for tag in tags.split(',').map(|x| x.parse::<u16>().unwrap()) {
                let i = bones.iter().position(|b| b.tag == tag).unwrap();
                let mut ms = crate::model::placed_meshes(std::path::Path::new(path), crate::model::LodLevel::High).unwrap();
                crate::model::transform_meshes(&mut ms, world[i]);
                println!("on {} ({tag}) at {:?}: {:?}", bones[i].name, world[i].w_axis, bounds(&ms));
            }
        }
    }

    /// needs the installed game; skips itself without it
    #[test]
    fn every_prop_anchor_has_its_bone_and_the_head_is_at_head_height() {
        let Ok(g) = Game::open(None) else {
            eprintln!("skipped: GTA V not found");
            return;
        };
        for sex in [Sex::Male, Sex::Female] {
            let bones = freemode_skeleton(&g, sex).unwrap();
            for a in 0..13u8 {
                assert!(anchor_matrix(&bones, a).is_some(), "{sex:?} anchor {a}: bone {} missing", anchor_bone_tag(a));
            }
            // the ped stands with its origin at the hips: the head is well above, the feet well below
            let head = anchor_matrix(&bones, 0).unwrap().w_axis.z;
            let foot = anchor_matrix(&bones, 9).unwrap().w_axis.z;
            assert!(head > 0.5 && foot < -0.7, "{sex:?}: head at {head}, foot at {foot}");
        }
    }

    /// needs the installed game; skips itself without it
    #[test]
    fn both_freemode_bodies_come_out_of_the_game_with_their_textures() {
        let Ok(g) = Game::open(None) else {
            eprintln!("skipped: GTA V not found");
            return;
        };
        for sex in [Sex::Male, Sex::Female] {
            let parts = freemode_body(&g, sex).unwrap();
            assert_eq!(parts.iter().map(|p| p.component).collect::<Vec<_>>(), [0, 2, 3, 4, 6], "{sex:?}");
            for p in &parts {
                assert!(p.ydd.is_file() && p.ytd.as_ref().is_some_and(|t| t.is_file()), "{p:?}");
            }
        }
    }
}
