//! DLC levels: the game's DLC packs in the order their `setup2.xml` gives (`<order value>`), so
//! that "the game as of DLC X" is every pack up to X. A map file belongs to the pack whose folder it lies under (`dlcpacks/<name>/`).
use crate::{game::Game, index::{FileIndex, Filter}};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlcLevel {
    /// the pack's folder name (`mpbeach`)
    pub name: String,
    pub order: i64,
}

/// the pack a game path lies under, lowercase (`x64w.rpf/dlcpacks/mpbeach/dlc.rpf/...` is `mpbeach`; the pack's patches in the game's update archive,
/// `update/update.rpf/dlc_patch/mpbeach/...`, belong to it too)
pub fn dlc_name_of(path: &str) -> Option<String> {
    let lower = path.to_lowercase();
    let rest = lower.split("dlcpacks/").nth(1).or_else(|| lower.split("dlc_patch/").nth(1))?;
    rest.split('/').next().map(|s| s.to_string())
}

/// The key of an archive that holds map data: its path from `levels/` on, lowercase, whatever device or platform prefix it has, so that
/// `platform:/levels/gta5/a.rpf`, `dlcMPHeist:/%PLATFORM%/levels/gta5/a.rpf` and `x64i.rpf/levels/gta5/a.rpf` are one archive: a pack's copy replaces the game's.
pub fn archive_key(path: &str) -> Option<String> {
    let lower = path.replace('\\', "/").to_lowercase();
    lower.find("levels/").map(|at| lower[at..].to_string())
}

/// One thing a pack's `content.xml` does to the map archives of the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapEvent {
    /// the game's archive (and every earlier pack's copy of it) is taken out
    Invalidate(String),
    /// the pack's own copy is put in
    Enable(String),
}

/// What a pack's `content.xml` does to the map archives, in the order they apply: for each content change set its own
/// `filesToEnable`, then for each `mapChangeSetData` item the archives it invalidates and the ones it enables. Only archives (`.rpf`) under `levels/` count.
pub fn parse_map_events(xml: &str) -> Vec<MapEvent> {
    let Ok(doc) = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')) else { return vec![] };
    let list = |n: roxmltree::Node, name: &str| -> Vec<String> {
        n.children().filter(|c| c.has_tag_name(name)).flat_map(|c| c.children().filter(|i| i.has_tag_name("Item")).filter_map(|i| i.text()).map(|t| t.trim().to_string()).collect::<Vec<_>>()).collect()
    };
    let key = |f: &str| archive_key(f).filter(|k| k.ends_with(".rpf"));
    let mut out = vec![];
    for sets in doc.descendants().filter(|n| n.has_tag_name("contentChangeSets")) {
        for set in sets.children().filter(|c| c.has_tag_name("Item")) {
            out.extend(list(set, "filesToEnable").iter().filter_map(|f| key(f)).map(MapEvent::Enable));
            for maps in set.children().filter(|c| c.has_tag_name("mapChangeSetData")) {
                for m in maps.children().filter(|c| c.has_tag_name("Item")) {
                    out.extend(list(m, "filesToInvalidate").iter().filter_map(|f| key(f)).map(MapEvent::Invalidate));
                    out.extend(list(m, "filesToEnable").iter().filter_map(|f| key(f)).map(MapEvent::Enable));
                }
            }
        }
    }
    out
}

/// Packs that are left out unless they are chosen (they break the map): this one carries base-named copies of maps that the
/// heists and lowriders packs replace under other names, so both versions would be in the world.
pub const LEFT_OUT_PACKS: [&str; 1] = ["patchday27ng"];

/// whether a game path lies in a pack that is left out
pub fn is_left_out(path: &str) -> bool {
    dlc_name_of(path).map_or(false, |n| LEFT_OUT_PACKS.contains(&n.as_str()))
}

/// What each pack's `content.xml` does to the map archives (the patched copy in the update archive when there is one), one list per level.
pub fn dlc_map_events(game: &Game, ix: &FileIndex, levels: &[DlcLevel]) -> Vec<Vec<MapEvent>> {
    let f = Filter { exts: vec!["xml".into()], ..Default::default() };
    let mut found: HashMap<String, String> = HashMap::new();
    for r in ix.find("content.xml", &f, false, 4000) {
        if !r.name().eq_ignore_ascii_case("content.xml") {
            continue;
        }
        let Some(name) = dlc_name_of(&r.path) else { continue };
        if r.path.to_lowercase().contains("dlc_patch/") || !found.contains_key(&name) {
            found.insert(name, r.path.clone());
        }
    }
    levels.iter().map(|l| found.get(&l.name).and_then(|p| game.read(p).ok()).map(|b| parse_map_events(&String::from_utf8_lossy(&b))).unwrap_or_default()).collect()
}

/// Which map files are in the game at each DLC level: the game's own archives, then each pack's in order, a pack's change sets taking archives out and its own in.
/// A pack that replaces an archive with its own copy (the heists update's `hei_` maps, the lowriders' `lr_` ones) leaves one version
/// of the area, not two.
#[derive(Debug, Clone, Default)]
pub struct DlcMaps {
    /// per map file: the pack it lies in (an index into the levels) and the key of its archive
    src: Vec<(Option<usize>, Option<String>)>,
    /// per level: what its pack's content.xml does
    events: Vec<Vec<MapEvent>>,
}

impl DlcMaps {
    /// `maps` are the game paths of the map files; `levels` the packs in order; `events[i]` what pack `i` does.
    pub fn new(maps: &[String], levels: &[DlcLevel], events: Vec<Vec<MapEvent>>) -> Self {
        let src = maps
            .iter()
            .map(|path| {
                let pack = dlc_name_of(path).and_then(|n| levels.iter().position(|l| l.name == n));
                let lower = path.to_lowercase();
                let key = lower.rfind(".rpf/").and_then(|i| archive_key(&lower[..i + 4]));
                (pack, key)
            })
            .collect();
        Self { src, events }
    }

    /// For each map file whether it is not in the game as of `level` (an index into the levels; `None` is the latest, with every pack).
    pub fn hidden(&self, level: Option<usize>) -> Vec<bool> {
        // who provides each archive key
        let mut have: HashMap<&str, HashSet<Option<usize>>> = HashMap::new();
        for (pack, key) in &self.src {
            if let Some(k) = key {
                have.entry(k).or_default().insert(*pack);
            }
        }
        let mut active: HashMap<&str, HashSet<Option<usize>>> = HashMap::new();
        for (k, packs) in &have {
            if packs.contains(&None) {
                active.entry(k).or_default().insert(None);
            }
        }
        let last = level.map_or(self.events.len(), |l| (l + 1).min(self.events.len()));
        for p in 0..last {
            for (k, packs) in &have {
                if packs.contains(&Some(p)) {
                    active.entry(k).or_default().insert(Some(p));
                }
            }
            for e in &self.events[p] {
                match e {
                    MapEvent::Invalidate(k) => {
                        active.remove(k.as_str());
                    }
                    MapEvent::Enable(k) => {
                        if let Some((k, _)) = have.get_key_value(k.as_str()).filter(|(_, v)| v.contains(&Some(p))) {
                            active.entry(k).or_default().insert(Some(p));
                        }
                    }
                }
            }
        }
        self.src
            .iter()
            .map(|(pack, key)| {
                if pack.map_or(false, |p| level.map_or(false, |l| p > l)) {
                    return true;
                }
                match key {
                    Some(k) => !active.get(k.as_str()).map_or(false, |v| v.contains(pack)),
                    None => false,
                }
            })
            .collect()
    }
}

/// the order value of a `setup2.xml`
pub fn parse_order(xml: &str) -> Option<i64> {
    let at = xml.find("<order")?;
    let rest = &xml[at..];
    let v = rest.split("value=\"").nth(1)?.split('"').next()?;
    v.trim().parse().ok()
}

/// every DLC pack of the install, earliest first
pub fn dlc_levels(game: &Game, ix: &FileIndex) -> Vec<DlcLevel> {
    let f = Filter { exts: vec!["xml".into()], ..Default::default() };
    let mut out: Vec<DlcLevel> = vec![];
    for r in ix.find("setup2.xml", &f, false, 1000) {
        if !r.name().eq_ignore_ascii_case("setup2.xml") {
            continue;
        }
        let (Some(name), Ok(bytes)) = (dlc_name_of(&r.path), game.read(&r.path)) else { continue };
        let order = parse_order(&String::from_utf8_lossy(&bytes)).unwrap_or(i64::MAX);
        if !out.iter().any(|d| d.name == name) {
            out.push(DlcLevel { name, order });
        }
    }
    out.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_are_found_in_paths_and_orders_in_setup_files() {
        assert_eq!(dlc_name_of("x64w.rpf/dlcpacks/MPBeach/dlc.rpf/x64/levels/a.ymap"), Some("mpbeach".into()));
        assert_eq!(dlc_name_of("x64a.rpf/levels/gta5/a.ymap"), None);
        assert_eq!(parse_order("<SSetupData><type>X</type><order value=\"1000\"/></SSetupData>"), Some(1000));
        assert_eq!(parse_order("<SSetupData/>"), None);
    }

    #[test]
    fn a_packs_patches_belong_to_it_and_archives_have_one_key_whoever_holds_them() {
        assert_eq!(dlc_name_of("update/update.rpf/dlc_patch/mpheist/x64/levels/a.rpf/b.ymap"), Some("mpheist".into()));
        let k = Some("levels/gta5/_citye/dt.rpf".to_string());
        assert_eq!(archive_key("platform:/levels/gta5/_citye/dt.rpf"), k);
        assert_eq!(archive_key("dlcMPHeist:/%PLATFORM%/levels/gta5/_citye/dt.rpf"), k);
        assert_eq!(archive_key("x64i.rpf\\levels\\gta5\\_citye\\dt.rpf"), k);
        assert_eq!(archive_key("update/x64/dlcpacks/mpheist/dlc.rpf/x64/levels/gta5/_citye/dt.rpf"), k);
        assert_eq!(archive_key("common.rpf/data/a.rpf"), None);
    }

    #[test]
    fn map_changes_are_read_in_the_order_they_apply() {
        let xml = r#"<?xml version="1.0"?><X><contentChangeSets>
            <Item><changeSetName>CARS</changeSetName><filesToEnable><Item>dlcX:/%PLATFORM%/levels/gta5/cars.rpf</Item><Item>dlcX:/x64/data/not_a_map.dat</Item></filesToEnable></Item>
            <Item><changeSetName>MAP</changeSetName><mapChangeSetData><Item><associatedMap>M</associatedMap>
              <filesToInvalidate><Item>platform:/levels/gta5/a/a.rpf</Item><Item>some.interior</Item></filesToInvalidate>
              <filesToDisable/>
              <filesToEnable><Item>dlcX:/%PLATFORM%/levels/gta5/a/a.rpf</Item></filesToEnable></Item></mapChangeSetData></Item>
            </contentChangeSets></X>"#;
        assert_eq!(
            parse_map_events(xml),
            vec![MapEvent::Enable("levels/gta5/cars.rpf".into()), MapEvent::Invalidate("levels/gta5/a/a.rpf".into()), MapEvent::Enable("levels/gta5/a/a.rpf".into())]
        );
        assert!(parse_map_events("not xml").is_empty());
    }

    #[test]
    fn a_pack_that_replaces_an_archive_leaves_one_version_of_it_and_a_level_before_it_has_the_old_one() {
        let levels = vec![DlcLevel { name: "heist".into(), order: 1 }, DlcLevel { name: "lowrider".into(), order: 2 }, DlcLevel { name: "patch".into(), order: 3 }];
        let maps: Vec<String> = [
            "x64i.rpf/levels/gta5/c/c_meta.rpf/c.ymap",                                    // 0 the game's
            "x64i.rpf/levels/gta5/d/d_meta.rpf/d.ymap",                                    // 1 the game's, never replaced
            "update/x64/dlcpacks/heist/dlc.rpf/x64/levels/gta5/c/c_meta.rpf/hei_c.ymap",   // 2 heist's copy
            "update/x64/dlcpacks/lowrider/dlc.rpf/x64/levels/gta5/c/c_meta.rpf/lr_c.ymap", // 3 lowrider's copy
            "update/x64/dlcpacks/patch/dlc.rpf/x64/levels/gta5/d/d_meta.rpf/d.ymap",       // 4 a later patch of d (an overlay: no change set)
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let inv = |k: &str| MapEvent::Invalidate(k.into());
        let en = |k: &str| MapEvent::Enable(k.into());
        let c = "levels/gta5/c/c_meta.rpf";
        let d = DlcMaps::new(&maps, &levels, vec![vec![inv(c), en(c)], vec![inv(c), en(c)], vec![]]);
        // latest: only the lowriders' version of c; d is the game's and the patch's (both stay: same names are settled by the loader)
        assert_eq!(d.hidden(None), vec![true, false, true, false, false]);
        // as of the heists: the heists' copy replaces the game's, the lowriders' and the patch do not exist yet
        assert_eq!(d.hidden(Some(0)), vec![true, false, false, true, true]);
    }
}
