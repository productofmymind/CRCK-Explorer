//! The "mods" folder, the way classic modding tools uses it: a copy of one of the game's archives (or loose files) at the same path under
//! `<game>\mods`. With the mods-folder ASI plugin and an ASI loader installed, the game reads the copy instead of its own file, so the
//! game's own files are never changed. CRCK Explorer only makes the copy; it ships no ASI and does not load anything into the game.
use anyhow::{bail, Context, Result};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// `<game>\mods`, where classic modding tools looks for them.
pub fn default_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("mods")
}

/// lowercase, `/`-separated, no trailing separator: Windows paths compare without regard to case
fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn is_under(path: &Path, dir: &Path) -> bool {
    let (p, d) = (norm(path), norm(dir));
    p == d || p.strip_prefix(&d).is_some_and(|rest| rest.starts_with('/'))
}

/// `path` relative to the game folder when it is one of the game's own files: inside the game folder and not inside the
/// mods folder. None for everything else (the mods copies, the user's own folders).
pub fn game_relative(game_dir: &Path, mods_dir: &Path, path: &Path) -> Option<PathBuf> {
    if !is_under(path, game_dir) || is_under(path, mods_dir) {
        return None;
    }
    let skip = Path::new(&norm(game_dir)).components().count();
    let rel: PathBuf = path.components().skip(skip).collect();
    (!rel.as_os_str().is_empty()).then_some(rel)
}

/// Where one of the game's own files goes in the mods folder (the same path below it), or None when `path` is not one.
pub fn target(game_dir: &Path, mods_dir: &Path, path: &Path) -> Option<PathBuf> {
    game_relative(game_dir, mods_dir, path).map(|rel| mods_dir.join(rel))
}

/// What [`copy_to_mods`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Copied {
    /// the copy was made
    New(PathBuf),
    /// a copy was there already and was left as it is (it may hold the user's changes)
    Existing(PathBuf),
}

impl Copied {
    pub fn path(&self) -> &Path {
        match self {
            Copied::New(p) | Copied::Existing(p) => p,
        }
    }
}

/// Copy one of the game's own files into the mods folder. An existing copy is never overwritten. The data goes to a
/// `.part` file first and is renamed when complete, so a failed or cancelled copy never leaves a broken archive in mods.
/// `progress(done, total)` is called as the copy goes; returning false cancels it.
pub fn copy_to_mods(game_dir: &Path, mods_dir: &Path, path: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<Copied> {
    let Some(dest) = target(game_dir, mods_dir, path) else {
        bail!("{} is not one of the game's own files (it is outside {} or already in the mods folder)", path.display(), game_dir.display());
    };
    if dest.exists() {
        return Ok(Copied::Existing(dest));
    }
    let parent = dest.parent().context("no parent folder")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let res = (|| -> Result<()> {
        let mut src = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let total = src.metadata()?.len();
        let mut out = fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        let mut buf = vec![0u8; 8 << 20];
        let mut done = 0u64;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).with_context(|| format!("writing {} (is the disk full?)", part.display()))?;
            done += n as u64;
            if !progress(done, total) {
                bail!("cancelled");
            }
        }
        out.sync_all()?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &dest).with_context(|| format!("renaming {} to {}", part.display(), dest.display()))?;
    Ok(Copied::New(dest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_files_are_told_from_mods_copies_and_other_folders() {
        let game = Path::new(r"E:\Games\GTA V");
        let mods = default_dir(game);
        assert_eq!(game_relative(game, &mods, Path::new(r"E:\Games\GTA V\x64e.rpf")), Some(PathBuf::from("x64e.rpf")));
        assert_eq!(game_relative(game, &mods, Path::new(r"e:\games\gta v\update\update.rpf")), Some(Path::new("update").join("update.rpf")), "case does not matter");
        assert_eq!(game_relative(game, &mods, Path::new(r"E:\Games\GTA V\mods\x64e.rpf")), None, "a mods copy is not a game file");
        assert_eq!(game_relative(game, &mods, Path::new(r"E:\Games\GTA V2\x64e.rpf")), None, "a folder that only starts with the same name");
        assert_eq!(game_relative(game, &mods, Path::new(r"D:\Mods\x64e.rpf")), None);
        assert_eq!(game_relative(game, &mods, game), None, "the game folder itself");
        assert_eq!(target(game, &mods, Path::new(r"E:\Games\GTA V\update\x64\dlcpacks\mpheist\dlc.rpf")), Some(mods.join(r"update\x64\dlcpacks\mpheist\dlc.rpf")));
        let elsewhere = Path::new(r"F:\MyMods");
        assert_eq!(target(game, elsewhere, Path::new(r"E:\Games\GTA V\x64e.rpf")), Some(elsewhere.join("x64e.rpf")), "a mods folder outside the game");
    }

    #[test]
    fn copying_makes_the_copy_once_and_never_overwrites_it() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_mods_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let game = d.join("GTA V");
        fs::create_dir_all(game.join("update")).unwrap();
        let src = game.join("update").join("update.rpf");
        fs::write(&src, vec![7u8; 100_000]).unwrap();
        let mods = default_dir(&game);
        let mut calls = 0;
        let made = copy_to_mods(&game, &mods, &src, &mut |done, total| {
            calls += 1;
            assert!(done <= total);
            true
        })
        .unwrap();
        assert_eq!(made, Copied::New(mods.join("update").join("update.rpf")));
        assert!(calls > 0);
        assert_eq!(fs::read(made.path()).unwrap(), fs::read(&src).unwrap());
        // the user's changes in the copy survive a second copy
        fs::write(made.path(), b"edited").unwrap();
        assert_eq!(copy_to_mods(&game, &mods, &src, &mut |_, _| true).unwrap(), Copied::Existing(made.path().to_path_buf()));
        assert_eq!(fs::read(made.path()).unwrap(), b"edited");
        // a cancelled copy leaves nothing behind
        let other = game.join("x64e.rpf");
        fs::write(&other, vec![1u8; 10]).unwrap();
        assert!(copy_to_mods(&game, &mods, &other, &mut |_, _| false).is_err());
        assert!(!mods.join("x64e.rpf").exists() && !mods.join("x64e.rpf.part").exists());
        // a mods copy or a file elsewhere is refused
        assert!(copy_to_mods(&game, &mods, made.path(), &mut |_, _| true).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
