//! The few things CRCK Explorer remembers between runs, in a small `key=value` file next to the cache folder.
use std::{fs, path::PathBuf};

/// `%LOCALAPPDATA%\CRCK Explorer\settings.txt`
pub fn path() -> PathBuf {
    rage_toolkit_core::game::cache_dir().parent().map(|p| p.join("settings.txt")).unwrap_or_else(|| PathBuf::from("rage_toolkit_settings.txt"))
}

/// Whether the Home screen opens at start (it does unless it was switched off).
pub fn show_home(path: &std::path::Path) -> bool {
    fs::read_to_string(path).map(|t| !t.lines().any(|l| l.trim().replace(' ', "") == "show_home=false")).unwrap_or(true)
}

pub fn save_show_home(path: &std::path::Path, on: bool) {
    rewrite(path, "show_home", vec![format!("show_home={on}")]);
}

/// Whether CRCK Explorer warns when you work in the game's own files instead of the "mods" folder (on unless switched off).
pub fn warn_game_files(path: &std::path::Path) -> bool {
    get(path, "warn_game_files").map(|v| v != "false").unwrap_or(true)
}

/// The "mods" folder chosen in Settings; None = the default, `<game>\mods`.
pub fn mods_dir(path: &std::path::Path) -> Option<PathBuf> {
    get(path, "mods_dir").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// One remembered value (`key=value`).
pub fn get(path: &std::path::Path, key: &str) -> Option<String> {
    fs::read_to_string(path).ok()?.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string()))
}

pub fn set(path: &std::path::Path, key: &str, value: &str) {
    rewrite(path, key, vec![format!("{key}={value}")]);
}

/// The folders added to the Explorer (`folder=<path>` lines), in the order they were added.
pub fn folders(path: &std::path::Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(path).unwrap_or_default();
    text.lines().filter_map(|l| l.strip_prefix("folder=")).filter(|p| !p.trim().is_empty()).map(|p| PathBuf::from(p.trim())).collect()
}

pub fn save_folders(path: &std::path::Path, dirs: &[PathBuf]) {
    rewrite(path, "folder", dirs.iter().map(|d| format!("folder={}", d.display())).collect());
}

/// Replace every line of one key with `new`, keeping the other keys as they are.
fn rewrite(path: &std::path::Path, key: &str, new: Vec<String>) {
    let old = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = old.lines().filter(|l| l.split('=').next().map(|k| k.trim() != key).unwrap_or(true) && !l.trim().is_empty()).map(str::to_string).collect();
    lines.extend(new);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, lines.join("\n") + "\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_screen_is_on_unless_switched_off() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_settings_{}", std::process::id()));
        let f = dir.join("settings.txt");
        assert!(show_home(&f), "no file: on");
        save_show_home(&f, false);
        assert!(!show_home(&f));
        save_show_home(&f, true);
        assert!(show_home(&f));
        std::fs::write(&f, "garbage\n").unwrap();
        assert!(show_home(&f));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn folders_are_remembered_beside_the_other_switches() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_settings_f_{}", std::process::id()));
        let f = dir.join("settings.txt");
        assert!(folders(&f).is_empty());
        save_show_home(&f, false);
        save_folders(&f, &[PathBuf::from("D:\\Mods\\A"), PathBuf::from("E:\\B")]);
        assert_eq!(folders(&f), vec![PathBuf::from("D:\\Mods\\A"), PathBuf::from("E:\\B")]);
        assert!(!show_home(&f), "the switch survives saving folders");
        save_show_home(&f, true);
        assert_eq!(folders(&f).len(), 2, "folders survive saving the switch");
        save_folders(&f, &[]);
        assert!(folders(&f).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_game_files_warning_is_on_until_switched_off() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_settings_w_{}", std::process::id()));
        let f = dir.join("settings.txt");
        assert!(warn_game_files(&f) && mods_dir(&f).is_none(), "no file: warn, default mods folder");
        set(&f, "warn_game_files", "false");
        set(&f, "mods_dir", "F:\\MyMods");
        assert!(!warn_game_files(&f));
        assert_eq!(mods_dir(&f), Some(PathBuf::from("F:\\MyMods")));
        set(&f, "mods_dir", "");
        assert_eq!(mods_dir(&f), None, "an empty value is the default again");
        let _ = std::fs::remove_dir_all(dir);
    }
}
