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

#[cfg(test)]
mod tests {
    use super::*;

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
