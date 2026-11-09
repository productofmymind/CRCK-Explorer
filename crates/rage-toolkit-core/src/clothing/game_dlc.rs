//! Rockstar's clothing DLCs from the game install: which there are (the YMT the game uses for each: DLC packs load in the order
//! of `dlclist.xml`, and a later pack's file stands over an earlier one's, as `patch2023_01` does for `mp_m_heist3`), and adding
//! one to a project as a Rockstar DLC. Its slots are the game's own (empty paths, so the build streams nothing for them); what the
//! user adds goes after them, and the build writes a replacement YMT.
use super::{parse_name, ymt, Dlc, Drawable, Kind, Project, Role, Sex, Texture};
use crate::{game::Game, index::FileIndex};
use anyhow::{bail, Context, Result};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

/// One of the game's clothing DLCs (both sexes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameDlc {
    /// the DLC name as the game spells it, without the sex: `heist3`, `2023_02`
    pub label: String,
    /// per sex (male, female): the YMT the game uses (a full game path), `None` when that sex has none
    pub ymt: [Option<String>; 2],
    /// per sex: how many packs have a YMT of it (more than one: a patch pack replaced it)
    pub versions: [usize; 2],
    /// the creature metadata the game has for it: (full game path, the name the shop meta uses)
    pub creature: Option<(String, String)>,
}

impl GameDlc {
    /// the DLC name the game uses for `sex`: `mp_m_heist3`
    pub fn name(&self, sex: Sex) -> String {
        format!("{}{}", if sex == Sex::Male { "mp_m_" } else { "mp_f_" }, self.label)
    }

    /// the DLC pack its YMT for `sex` comes from (`mpheist3`, `patch2023_01`)
    pub fn pack(&self, sex: Sex) -> Option<String> {
        self.ymt[sex_i(sex)].as_deref().and_then(pack_of)
    }
}

fn sex_i(sex: Sex) -> usize {
    usize::from(sex == Sex::Female)
}

/// the DLC pack a game path is in: `update/x64/dlcpacks/<pack>/dlc.rpf/...`
fn pack_of(path: &str) -> Option<String> {
    let l = path.to_lowercase();
    let rest = &l[l.find("dlcpacks/")? + 9..];
    Some(rest.split('/').next()?.to_string())
}

/// The order the game mounts DLC packs in (`dlcpacks:/name/` items of `dlclist.xml`, the title update's first): pack -> position.
pub fn pack_order(dlclist: &str) -> HashMap<String, usize> {
    let mut order = HashMap::new();
    for (n, item) in dlclist.split("<Item>").skip(1).enumerate() {
        let item = item.split("</Item>").next().unwrap_or("").trim().to_lowercase();
        if let Some(name) = item.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()) {
            order.entry(name.to_string()).or_insert(n);
        }
    }
    order
}

/// `mp_m_freemode_01_mp_m_heist3.ymt` -> (male, `heist3`); the freemode ped's own `mp_m_freemode_01.ymt` and other files -> None
fn ymt_name(file: &str) -> Option<(Sex, String)> {
    let stem = file.to_lowercase().strip_suffix(".ymt")?.to_string();
    let (sex, dlc) = if let Some(d) = stem.strip_prefix("mp_m_freemode_01_mp_m_") {
        (Sex::Male, d)
    } else if let Some(d) = stem.strip_prefix("mp_f_freemode_01_mp_f_") {
        (Sex::Female, d)
    } else {
        return None;
    };
    (!dlc.is_empty()).then(|| (sex, dlc.to_string()))
}

/// The game's clothing DLCs, by name, from the file index (`order`: [`pack_order`]). Of several YMTs of one DLC and sex the one
/// in the pack mounted last is the one the game uses.
pub fn list(index: &FileIndex, order: &HashMap<String, usize>) -> Vec<GameDlc> {
    let rank = |path: &str| pack_of(path).and_then(|p| order.get(&p).copied()).map(|r| r + 1).unwrap_or(0);
    let mut by: BTreeMap<String, GameDlc> = BTreeMap::new();
    let mut creatures: HashMap<String, (usize, String)> = HashMap::new();
    for r in index.records() {
        let name = r.name();
        if let Some((sex, label)) = ymt_name(name) {
            let g = by.entry(label.clone()).or_insert_with(|| GameDlc { label, ..Default::default() });
            let s = sex_i(sex);
            g.versions[s] += 1;
            if g.ymt[s].as_deref().is_none_or(|have| rank(&r.path) >= rank(have)) {
                g.ymt[s] = Some(r.path.clone());
            }
        } else if let Some(label) = name.to_lowercase().strip_prefix("mp_creaturemetadata_").and_then(|n| n.strip_suffix(".ymt")) {
            let k = rank(&r.path);
            if creatures.get(label).is_none_or(|(have, _)| k >= *have) {
                creatures.insert(label.to_string(), (k, r.path.clone()));
            }
        }
    }
    for g in by.values_mut() {
        if let Some((_, path)) = creatures.get(&g.label) {
            g.creature = Some((path.clone(), format!("MP_CreatureMetadata_{}", g.label)));
        }
    }
    by.into_values().collect()
}

/// [`list`] from the game: its file index (made once, then cached; returned too) and its `dlclist.xml`.
pub fn from_game(game: &Game, job: &crate::job::Job) -> Result<(Vec<GameDlc>, FileIndex)> {
    let index = FileIndex::load_or_build(game, job, false).context("listing the game's files")?;
    let dlclist = ["update/update.rpf/common/data/dlclist.xml", "common.rpf/data/dlclist.xml"].iter().find_map(|p| game.read(p).ok()).context("the game's dlclist.xml was not found")?;
    Ok((list(&index, &pack_order(&String::from_utf8_lossy(&dlclist))), index))
}

/// The game files [`make_rockstar`] and [`add`] read for `g` (its YMTs and creature metadata), read now, so the change itself can
/// be made later without the game (`read` of the result).
pub fn game_files(game: &Game, g: &GameDlc) -> Result<HashMap<String, Vec<u8>>> {
    let mut out = HashMap::new();
    for path in g.ymt.iter().flatten().chain(g.creature.as_ref().map(|c| &c.0)) {
        out.insert(path.clone(), game.read(path).with_context(|| format!("reading {path} from the game"))?);
    }
    Ok(out)
}

/// The race of each drawable and texture of a DLC's files in the game (`mp_m_freemode_01_mp_m_heist3/jbib_000_r.ydd`): the
/// slots made for it are named as the game's files, so a model put in one later replaces the right file.
#[derive(Debug, Default)]
pub struct Races {
    models: HashMap<(Kind, u32), String>,
    textures: HashMap<(Kind, u32, char), String>,
}

impl Races {
    pub fn of(index: &FileIndex, g: &GameDlc, sex: Sex) -> Self {
        let dlc = g.name(sex);
        let ped = sex.ped();
        let folders = [format!("{ped}_{dlc}"), format!("{ped}_p_{dlc}")];
        let mut r = Races::default();
        for rec in index.records() {
            let Some((dir, file)) = rec.path.rsplit_once('/') else { continue };
            let folder = dir.rsplit('/').next().unwrap_or("");
            if !folders.iter().any(|f| f.eq_ignore_ascii_case(folder)) {
                continue;
            }
            match parse_name(file) {
                Some(Role::Drawable { kind, number, race }) => {
                    r.models.insert((kind, number), race);
                }
                Some(Role::Texture { kind, number, letter, race }) => {
                    r.textures.insert((kind, number, letter), race);
                }
                _ => {}
            }
        }
        r
    }
}

/// The slots of one sex of a Rockstar DLC as its YMT has them: every drawable with empty paths (the game's own files), its
/// textures, alternates and cloth counted as the YMT counts them, and its settings.
pub fn slots(y: &ymt::Ymt, dlc: usize, sex: Sex, races: &Races) -> Vec<Drawable> {
    let mut out = vec![];
    for (kind, number, entry) in y.entries() {
        let mut d = Drawable { dlc, sex, kind, number, source_number: number, ..Default::default() };
        d.race = match kind {
            Kind::Prop(_) => String::new(),
            // the game's file name, else what the YMT says (bit 16 of propMask is a _r drawable)
            _ => races.models.get(&(kind, number)).cloned().unwrap_or_else(|| if entry.prop_mask & 16 != 0 { "r".into() } else { "u".into() }),
        };
        let tex_race = if kind.is_prop() { String::new() } else if d.race == "r" { "whi".into() } else { "uni".into() };
        for n in 0..entry.textures.len() {
            let letter = (b'a' + n.min(25) as u8) as char;
            let race = races.textures.get(&(kind, number, letter)).cloned().unwrap_or_else(|| tex_race.clone());
            d.textures.push(Texture { path: PathBuf::new(), letter, race, tex_id: None, edit: None });
        }
        d.alternates = (1..=u32::from(entry.alternates)).map(|n| (n, PathBuf::new())).collect();
        if entry.cloth {
            d.cloth = Some(PathBuf::new());
        }
        entry.apply(&mut d);
        out.push(d);
    }
    out
}

/// What the game has of a Rockstar DLC: its YMT per sex and its creature metadata entries.
struct FromGame {
    ymts: [Option<ymt::Ymt>; 2],
    creature: Option<Vec<super::extras::Expression>>,
}

fn read_game(g: &GameDlc, read: &dyn Fn(&str) -> Result<Vec<u8>>) -> Result<FromGame> {
    let mut ymts: [Option<ymt::Ymt>; 2] = [None, None];
    for sex in [Sex::Male, Sex::Female] {
        let Some(path) = &g.ymt[sex_i(sex)] else { continue };
        let bytes = read(path).with_context(|| format!("reading {path}"))?;
        let (y, hash, odd) = ymt::read_ymt(&bytes).with_context(|| format!("reading {path}"))?;
        if let Some(o) = odd.first() {
            bail!("{path} holds what the builder does not model ({o}); it cannot be replaced without losing that");
        }
        if hash != rage_formats::rage_joaat(&g.name(sex)) {
            // older DLCs: mp_m_freemode_01_mp_m_heist_01.ymt holds dlcName male_heist
            bail!("{path} calls its DLC something other than {} inside (an older DLC; hash {hash:08X}), which the builder cannot keep yet", g.name(sex));
        }
        ymts[sex_i(sex)] = Some(y);
    }
    if ymts.iter().all(Option::is_none) {
        bail!("{} has no YMT in the game", g.label);
    }
    // the game's creature metadata, kept entry by entry
    let creature = match &g.creature {
        Some((path, _)) => Some(super::extras::read_creature(&read(path).with_context(|| format!("reading {path}"))?).with_context(|| format!("reading {path}"))?),
        None => None,
    };
    Ok(FromGame { ymts, creature })
}

/// refuse a Rockstar DLC another DLC of the project (not `except`) already is
fn check_unused(p: &Project, g: &GameDlc, except: Option<usize>) -> Result<()> {
    for sex in [Sex::Male, Sex::Female] {
        if let Some(k) = (0..p.dlcs.len()).find(|&k| Some(k) != except && p.dlc_name(k, sex).eq_ignore_ascii_case(&g.name(sex))) {
            bail!("the project already has {} (DLC {})", g.name(sex), k + 1);
        }
    }
    Ok(())
}

fn rockstar_dlc(g: &GameDlc, creature: Option<Vec<super::extras::Expression>>) -> Dlc {
    Dlc { names: Some((g.name(Sex::Male), g.name(Sex::Female))), creature: g.creature.as_ref().map(|c| c.1.clone()), base: true, game_creature: creature, ..Default::default() }
}

/// Add Rockstar DLC `g` to the project: a Rockstar DLC with the game's names, its creature metadata name, and a slot for every
/// drawable of its YMTs (read from the game). `read` gives a game file's bytes. Returns the new DLC's index.
pub fn add(p: &mut Project, g: &GameDlc, read: &dyn Fn(&str) -> Result<Vec<u8>>, races: [&Races; 2]) -> Result<usize> {
    check_unused(p, g, None)?;
    let got = read_game(g, read)?;
    let k = p.dlcs.len();
    p.dlcs.push(rockstar_dlc(g, got.creature));
    for sex in [Sex::Male, Sex::Female] {
        if let Some(y) = &got.ymts[sex_i(sex)] {
            p.drawables.extend(slots(y, k, sex, races[sex_i(sex)]));
        }
    }
    Ok(k)
}

/// What making a DLC a Rockstar one did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Made {
    /// the game's slots put in front of the DLC's drawables
    pub slots: usize,
    /// the DLC's drawables, numbered after the game's
    pub kept: usize,
    /// drawables that no longer fitted (the game's and ours over the limit of one type): moved on to the next DLC
    pub moved_on: usize,
}

/// Make DLC `k` of the project Rockstar DLC `g`: the build then writes the game's YMT with the DLC's drawables added after the
/// game's own (renumbered per sex and type: the game's slots first), instead of a DLC of its own. What would go past the limit of
/// one type moves on to the next DLC. A DLC that is a Rockstar one already is made a new one first ([`make_new`]).
pub fn make_rockstar(p: &mut Project, k: usize, g: &GameDlc, read: &dyn Fn(&str) -> Result<Vec<u8>>, races: [&Races; 2]) -> Result<Made> {
    if k >= p.dlcs.len() {
        bail!("the project has no DLC {}", k + 1);
    }
    check_unused(p, g, Some(k))?;
    let got = read_game(g, read)?;
    if p.dlcs[k].base {
        make_new(p, k);
    }
    let mut made = Made::default();
    let mut game_slots: Vec<Drawable> = vec![];
    for sex in [Sex::Male, Sex::Female] {
        if let Some(y) = &got.ymts[sex_i(sex)] {
            game_slots.extend(slots(y, k, sex, races[sex_i(sex)]));
        }
    }
    // per sex and type, how many the game has: ours go after them
    let mut count: HashMap<(Sex, Kind), u32> = HashMap::new();
    for d in &game_slots {
        *count.entry((d.sex, d.kind)).or_default() += 1;
    }
    let limit = p.limit.max(1) as u32;
    let mut over: Vec<Drawable> = vec![];
    let (mine, rest): (Vec<Drawable>, Vec<Drawable>) = std::mem::take(&mut p.drawables).into_iter().partition(|d| d.dlc == k);
    p.drawables = rest;
    for mut d in mine {
        d.number += count.get(&(d.sex, d.kind)).copied().unwrap_or(0);
        if d.number >= limit {
            over.push(d);
        } else {
            made.kept += 1;
            p.drawables.push(d);
        }
    }
    made.slots = game_slots.len();
    p.drawables.extend(game_slots);
    p.dlcs[k] = rockstar_dlc(g, got.creature);
    p.drawables.sort_by_key(Drawable::slot);
    over.sort_by_key(Drawable::slot);
    made.moved_on = over.len();
    for d in over {
        let (dlc, number) = p.next_slot(k + 1, d.sex, d.kind);
        p.insert_sorted(Drawable { dlc, number, ..d });
    }
    Ok(made)
}

/// Make DLC `k` a DLC of the project's own again (`<project>_<n>`, its own YMT and shop meta): the game's slots (drawables with no
/// model in the pack) go, the others are numbered `0, 1, 2, ...` in their order. Returns how many slots went.
pub fn make_new(p: &mut Project, k: usize) -> usize {
    let Some(d) = p.dlcs.get_mut(k) else { return 0 };
    *d = Dlc::default();
    let before = p.drawables.len();
    p.drawables.retain(|d| !(d.dlc == k && d.ydd.as_os_str().is_empty()));
    let removed = before - p.drawables.len();
    p.drawables.sort_by_key(Drawable::slot);
    let mut next: HashMap<(Sex, Kind), u32> = HashMap::new();
    for d in p.drawables.iter_mut().filter(|d| d.dlc == k) {
        let n = next.entry((d.sex, d.kind)).or_insert(0);
        d.number = *n;
        *n += 1;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_mounted_pack_wins() {
        let order = pack_order("<Paths>\n<Item>dlcpacks:/mpheist3/</Item>\n<Item>dlcpacks:/patch2023_01/</Item>\n</Paths>");
        assert_eq!(order["mpheist3"], 0);
        assert_eq!(order["patch2023_01"], 1);
        let index = FileIndex::from_files(
            "t",
            [
                ("update/x64/dlcpacks/patch2023_01/dlc.rpf/x64/models/cdimages/patch2023_01_male.rpf/mp_m_freemode_01_mp_m_heist3.ymt", 1, "a"),
                ("update/x64/dlcpacks/mpheist3/dlc.rpf/x64/models/cdimages/mpheist3_male.rpf/mp_m_freemode_01_mp_m_heist3.ymt", 1, "b"),
                ("update/x64/dlcpacks/mpheist3/dlc.rpf/x64/models/cdimages/mpheist3_female.rpf/mp_f_freemode_01_mp_f_heist3.ymt", 1, "b"),
                ("update/x64/dlcpacks/mpheist3/dlc.rpf/x64/anim/creaturemetadata.rpf/mp_creaturemetadata_heist3.ymt", 1, "b"),
                ("x64v.rpf/models/cdimages/streamedpeds_mp.rpf/mp_m_freemode_01.ymt", 1, "c"),
            ]
            .map(|(p, s, t)| (p.to_string(), s, t.to_string())),
        );
        let l = list(&index, &order);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].label, "heist3");
        assert_eq!(l[0].pack(Sex::Male).as_deref(), Some("patch2023_01"));
        assert_eq!(l[0].pack(Sex::Female).as_deref(), Some("mpheist3"));
        assert_eq!(l[0].versions, [2, 1]);
        assert_eq!(l[0].creature.as_ref().unwrap().1, "MP_CreatureMetadata_heist3");
    }

    #[test]
    fn a_rockstar_dlc_comes_in_as_its_slots() {
        use crate::clothing::{build::plan_build, ymt::ymt_bytes};
        // a YMT as the game would have it: two jbibs (one _r with two textures) and a hat
        let mut src = Project::default();
        src.drawables.push(Drawable { kind: Kind::Component(11), number: 0, race: "u".into(), textures: vec![Texture { path: "x.ytd".into(), letter: 'a', race: "uni".into(), tex_id: None, edit: None }], ..Default::default() });
        src.drawables.push(Drawable { kind: Kind::Component(11), number: 1, race: "r".into(), textures: (0..2).map(|n| Texture { path: "x.ytd".into(), letter: (b'a' + n) as char, race: "whi".into(), tex_id: None, edit: None }).collect(), ..Default::default() });
        src.drawables.push(Drawable { kind: Kind::Prop(0), number: 0, textures: vec![Texture { path: "x.ytd".into(), letter: 'a', race: String::new(), tex_id: None, edit: None }], ..Default::default() });
        let refs: Vec<&Drawable> = src.drawables.iter().collect();
        let y = ymt::Ymt::of(&refs);
        let bytes = ymt_bytes(&y, "mp_m_heist3").unwrap();
        let g = GameDlc { label: "heist3".into(), ymt: [Some("m.ymt".into()), None], versions: [1, 0], creature: None };
        let mut p = Project { name: "pack".into(), ..Default::default() };
        let k = add(&mut p, &g, &|_| Ok(bytes.clone()), [&Races::default(), &Races::default()]).unwrap();
        assert!(p.dlcs[k].base);
        assert_eq!(p.dlc_name(k, Sex::Male), "mp_m_heist3");
        assert_eq!(p.drawables.len(), 3);
        assert_eq!(p.drawables[1].race, "r");
        assert_eq!(p.drawables[1].textures.len(), 2);
        assert!(p.drawables.iter().all(|d| d.ydd.as_os_str().is_empty()));
        // built as it is, the YMT is the game's again and nothing else is streamed for it
        let plan = plan_build(&p);
        assert_eq!(plan.errors(), 0, "{:#?}", plan.issues);
        let ours = plan.outputs.iter().find(|o| o.dest.ends_with("mp_m_freemode_01_mp_m_heist3.ymt")).unwrap();
        match &ours.source {
            crate::clothing::build::Source::Bytes(b) => assert_eq!(ymt::read_ymt(b).unwrap().0, y),
            _ => panic!(),
        }
        assert!(!plan.outputs.iter().any(|o| o.dest.ends_with(".ydd") || o.dest.ends_with(".ytd")));
        // switched off, nothing of it is written
        p.dlcs[k].left_out = true;
        let plan = plan_build(&p);
        assert!(!plan.outputs.iter().any(|o| o.dest.contains("heist3")), "{:#?}", plan.outputs.iter().map(|o| &o.dest).collect::<Vec<_>>());
        assert!(plan.issues.iter().any(|i| i.code == "CLOTH_DLC_LEFT_OUT"));
        // twice is refused
        assert!(add(&mut p, &g, &|_| Ok(bytes.clone()), [&Races::default(), &Races::default()]).is_err());
    }

    #[test]
    fn a_dlc_of_ours_becomes_a_rockstar_one_and_back() {
        use crate::clothing::ymt::ymt_bytes;
        // the game's DLC: two male jbibs
        let mut src = Project::default();
        for n in 0..2 {
            src.drawables.push(Drawable { kind: Kind::Component(11), number: n, race: "u".into(), textures: vec![Texture { path: "x.ytd".into(), letter: 'a', race: "uni".into(), tex_id: None, edit: None }], ..Default::default() });
        }
        let ymt = ymt_bytes(&ymt::Ymt::of(&src.drawables.iter().collect::<Vec<_>>()), "mp_m_2023_02").unwrap();
        let g = GameDlc { label: "2023_02".into(), ymt: [Some("m.ymt".into()), None], versions: [1, 0], creature: None };
        // ours: DLC 0 with three male jbibs (limit 4: the third goes past it) and a female one
        let mut p = Project { name: "pack".into(), limit: 4, ..Default::default() };
        for n in 0..3 {
            p.add_next(Drawable { kind: Kind::Component(11), ydd: format!("m{n}.ydd").into(), race: "u".into(), ..Default::default() });
        }
        p.add_next(Drawable { sex: Sex::Female, kind: Kind::Component(11), ydd: "f.ydd".into(), race: "u".into(), ..Default::default() });
        let ids: Vec<u64> = p.drawables.iter().map(|d| d.id).collect();
        let made = make_rockstar(&mut p, 0, &g, &|_| Ok(ymt.clone()), [&Races::default(), &Races::default()]).unwrap();
        assert_eq!(made, Made { slots: 2, kept: 3, moved_on: 1 });
        assert!(p.dlcs[0].base);
        assert_eq!(p.dlc_name(0, Sex::Male), "mp_m_2023_02");
        let at = |p: &Project, id: u64| p.find(id).map(|d| (d.dlc, d.number)).unwrap();
        // the game's 000, 001; ours 002, 003; the last over the limit, into DLC 1; the female one keeps 000 (the game has none)
        assert_eq!(at(&p, ids[0]), (0, 2));
        assert_eq!(at(&p, ids[1]), (0, 3));
        assert_eq!(at(&p, ids[2]), (1, 0));
        assert_eq!(p.drawables.iter().filter(|d| d.dlc == 0 && d.ydd.as_os_str().is_empty()).count(), 2);
        let female = p.drawables.iter().find(|d| d.sex == Sex::Female).unwrap();
        assert_eq!((female.dlc, female.number), (0, 0));
        // and back: the game's slots go, ours are 000, 001 again
        assert_eq!(make_new(&mut p, 0), 2);
        assert!(!p.dlcs[0].base);
        assert_eq!(p.dlc_name(0, Sex::Male), "pack_1");
        assert_eq!(at(&p, ids[0]), (0, 0));
        assert_eq!(at(&p, ids[1]), (0, 1));
        // the same Rockstar DLC twice is refused
        make_rockstar(&mut p, 0, &g, &|_| Ok(ymt.clone()), [&Races::default(), &Races::default()]).unwrap();
        assert!(make_rockstar(&mut p, 1, &g, &|_| Ok(ymt.clone()), [&Races::default(), &Races::default()]).is_err());
    }

    #[test]
    fn the_games_creature_metadata_is_kept_and_only_added_to() {
        use crate::clothing::{build::{plan_build, Source}, extras::{creature_bytes, read_creature, Expression}, ymt::ymt_bytes};
        let mut src = Project::default();
        for n in 0..2 {
            src.drawables.push(Drawable { kind: Kind::Component(6), number: n, race: "u".into(), textures: vec![Texture { path: "x.ytd".into(), letter: 'a', race: "uni".into(), tex_id: None, edit: None }], ..Default::default() });
        }
        let y = ymt::Ymt::of(&src.drawables.iter().collect::<Vec<_>>());
        let ymt = ymt_bytes(&y, "mp_m_heist3").unwrap();
        // the game's file: one odd entry for feet 1 (another bone), none for feet 0
        let odd = Expression { ids: 999u16.to_le_bytes().to_vec(), ..Expression::heels(1) };
        let creature = creature_bytes(&[odd.clone()]).unwrap();
        let g = GameDlc { label: "heist3".into(), ymt: [Some("m.ymt".into()), None], versions: [1, 0], creature: Some(("c.ymt".into(), "MP_CreatureMetadata_heist3".into())) };
        let mut p = Project { name: "pack".into(), ..Default::default() };
        let k = add(&mut p, &g, &|path| Ok(if path == "c.ymt" { creature.clone() } else { ymt.clone() }), [&Races::default(), &Races::default()]).unwrap();
        assert_eq!(p.dlcs[k].game_creature.as_deref(), Some(&[odd.clone()][..]));
        // nothing added: the game's file stays
        assert!(!plan_build(&p).outputs.iter().any(|o| o.dest.contains("creaturemetadata")));
        // a feet of ours after the game's two: the game's entry as it was, plus heels for ours
        let dir = std::env::temp_dir().join(format!("crck_game_creature_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (model, tex) = (dir.join("feet_000_u.ydd"), dir.join("feet_diff_000_a_uni.ytd"));
        std::fs::write(&model, b"m").unwrap();
        std::fs::write(&tex, b"t").unwrap();
        p.drawables.push(Drawable { dlc: k, kind: Kind::Component(6), number: 2, ydd: model, race: "u".into(), textures: vec![Texture { path: tex, letter: 'a', race: "uni".into(), tex_id: None, edit: None }], ..Default::default() });
        let plan = plan_build(&p);
        assert_eq!(plan.errors(), 0, "{:#?}", plan.issues);
        let o = plan.outputs.iter().find(|o| o.dest.ends_with("stream/mp_creaturemetadata_heist3.ymt")).expect("the creature file is written");
        let Source::Bytes(b) = &o.source else { panic!() };
        assert_eq!(read_creature(b).unwrap(), vec![odd, Expression::heels(2)]);
        // and it survives a save
        let f = dir.join("p.ragetoolkitclothes");
        crate::clothing::project::save(&p, &f).unwrap();
        assert_eq!(crate::clothing::project::load(&f).unwrap().dlcs[k], p.dlcs[k]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every clothing DLC of the installed game: added to a project, planned, its YMT read back the same, and the creature
    /// metadata the build would write compared with the game's.
    #[test]
    #[ignore = "needs the game: run with --release --ignored --nocapture"]
    fn every_game_dlc_comes_in_and_builds_back() {
        use crate::clothing::{build::{plan_build, Source}, extras};
        let game = Game::open(None).unwrap();
        let job = crate::job::Job::new();
        let index = FileIndex::load_or_build(&game, &job, false).unwrap();
        let t = std::time::Instant::now();
        let all = from_game(&game, &job).unwrap().0;
        println!("{} DLCs listed in {:.0} ms", all.len(), t.elapsed().as_secs_f64() * 1000.0);
        let (mut ok, mut same_creature, mut other_creature, mut failed) = (0, 0, vec![], vec![]);
        for g in &all {
            let races = [Races::of(&index, g, Sex::Male), Races::of(&index, g, Sex::Female)];
            let mut p = Project { name: "t".into(), ..Default::default() };
            let k = match add(&mut p, g, &|path| game.read(path), [&races[0], &races[1]]) {
                Ok(k) => k,
                Err(e) => {
                    failed.push(format!("{}: {e:#}", g.label));
                    continue;
                }
            };
            let plan = plan_build(&p);
            if plan.errors() > 0 {
                failed.push(format!("{}: {:?}", g.label, plan.issues.iter().filter(|i| i.sev == crate::clothing::Sev::Error).map(|i| &i.message).collect::<Vec<_>>()));
                continue;
            }
            for sex in [Sex::Male, Sex::Female] {
                let Some(path) = &g.ymt[sex_i(sex)] else { continue };
                let want = ymt::read_ymt(&game.read(path).unwrap()).unwrap().0;
                let o = plan.outputs.iter().find(|o| o.dest.ends_with(&format!("{}_{}.ymt", sex.ped(), g.name(sex)))).unwrap();
                let Source::Bytes(b) = &o.source else { panic!() };
                assert_eq!(ymt::read_ymt(b).unwrap().0, want, "{path}");
            }
            if let Some((path, _)) = &g.creature {
                let game_list = extras::read_creature(&game.read(path).unwrap()).unwrap();
                let ours = extras::creature_expressions(&p, k);
                assert!(!plan.outputs.iter().any(|o| o.dest.to_lowercase().contains("creaturemetadata")), "{}: nothing added, the game's creature file stays", g.label);
                if ours == game_list {
                    same_creature += 1;
                } else {
                    other_creature.push(format!("{}: game {} entries ({} comp, {} prop), ours {}", g.label, game_list.len(), game_list.iter().filter(|e| !e.prop).count(), game_list.iter().filter(|e| e.prop).count(), ours.len()));
                }
            }
            ok += 1;
        }
        println!("{ok} added and planned; creature metadata: {same_creature} the same, {} different", other_creature.len());
        for x in other_creature.iter().take(15) {
            println!("  creature {x}");
        }
        for f in &failed {
            println!("  failed {f}");
        }
        for g in all.iter().filter(|g| g.versions.iter().any(|v| *v > 1)).take(10) {
            println!("  {}: male {:?} ({}), female {:?} ({})", g.label, g.pack(Sex::Male), g.versions[0], g.pack(Sex::Female), g.versions[1]);
        }
    }
}
