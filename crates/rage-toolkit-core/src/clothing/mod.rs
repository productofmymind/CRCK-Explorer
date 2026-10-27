//! Clothing packs for GTA V freemode peds (FiveM): find the drawables (`.ydd`) and textures (`.ytd`) in dropped files and folders,
//! pair them by name, number them, check them, and (see `build`) write the YMT, the shop meta and the resource.
//! Formats and what was checked against the game's own files are described in the build module.
//!
//! Names (freemode): a component drawable is `jbib_000_u.ydd`, its textures `jbib_diff_000_a_uni.ytd`, `..._b_uni.ytd`, ...;
//! a prop is `p_head_000.ydd` with textures `p_head_diff_000_a.ytd`. The number is the drawable's index in its component, the letter
//! the texture's index (`a` = 0). In a finished addon the numbers of one component and sex run `0, 1, 2, ...` with no gap, because
//! the YMT stores drawables by position.
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

pub mod body;
pub mod build;
pub mod project;

/// The 12 ped components, in the game's order (the index is the `compIdx` of the YMT).
pub const COMPONENTS: [&str; 12] = ["head", "berd", "hair", "uppr", "lowr", "hand", "feet", "teef", "accs", "task", "decl", "jbib"];
/// The prop anchor points, in the game's order (`p_head_000` is anchor 0).
pub const ANCHORS: [&str; 9] = ["head", "eyes", "ears", "mouth", "lhand", "rhand", "lwrist", "rwrist", "hip"];
/// What a texture letter can count up to (`a`..`z`).
pub const MAX_TEXTURES: usize = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sex {
    Male,
    Female,
}

impl Sex {
    /// the ped model the clothes are for
    pub fn ped(self) -> &'static str {
        match self {
            Sex::Male => "mp_m_freemode_01",
            Sex::Female => "mp_f_freemode_01",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sex::Male => "male",
            Sex::Female => "female",
        }
    }
}

/// A component (`jbib`) or a prop anchor (`p_head`), by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Component(u8),
    Prop(u8),
}

impl Kind {
    /// `jbib` or `p_head`
    pub fn name(self) -> String {
        match self {
            Kind::Component(i) => COMPONENTS[i as usize].to_string(),
            Kind::Prop(i) => format!("p_{}", ANCHORS[i as usize]),
        }
    }

    pub fn is_prop(self) -> bool {
        matches!(self, Kind::Prop(_))
    }

    pub fn index(self) -> u8 {
        match self {
            Kind::Component(i) | Kind::Prop(i) => i,
        }
    }

    /// the file name of a drawable of this kind
    pub fn drawable_file(self, number: u32, race: &str) -> String {
        match self {
            Kind::Component(_) => format!("{}_{:03}_{}.ydd", self.name(), number, if race.is_empty() { "u" } else { race }),
            Kind::Prop(_) => format!("{}_{:03}.ydd", self.name(), number),
        }
    }

    /// the file name of one of its textures (`letter` is `a`..`z`)
    pub fn texture_file(self, number: u32, letter: char, race: &str) -> String {
        match self {
            Kind::Component(i) => format!("{}_diff_{:03}_{}_{}.ytd", COMPONENTS[i as usize], number, letter, if race.is_empty() { "uni" } else { race }),
            Kind::Prop(i) => format!("p_{}_diff_{:03}_{}.ytd", ANCHORS[i as usize], number, letter),
        }
    }

    /// every kind in the order the game lists them: components, then props
    pub fn all() -> Vec<Kind> {
        (0..12u8).map(Kind::Component).chain((0..9u8).map(Kind::Prop)).collect()
    }
}

// ---- reading names ----------------------------------------------------------------------------------------------------

/// What a file is, judged by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Drawable { kind: Kind, number: u32, race: String },
    Texture { kind: Kind, number: u32, letter: char, race: String },
    /// cloth physics (`.yld`) of a drawable
    Cloth { kind: Kind, number: u32 },
    /// an alternate model of a drawable (`jbib_007_u_1.ydd`: alternate 1)
    Alternate { kind: Kind, number: u32, alt: u32 },
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn component(s: &str) -> Option<Kind> {
    COMPONENTS.iter().position(|c| *c == s).map(|i| Kind::Component(i as u8))
}

fn anchor(s: &str) -> Option<Kind> {
    ANCHORS.iter().position(|a| *a == s).map(|i| Kind::Prop(i as u8))
}

/// Read a file name such as `jbib_000_u.ydd`, `jbib_diff_000_a_uni.ytd`, `p_head_diff_001_b.ytd` or `mp_m_freemode_01_pack^jbib_000_u.ydd`.
/// `None` when it is not a clothing file name.
pub fn parse_name(file_name: &str) -> Option<Role> {
    let lower = file_name.to_lowercase();
    let lower = lower.rsplit('^').next().unwrap_or(&lower);
    let (stem, ext) = lower.rsplit_once('.')?;
    let t: Vec<&str> = stem.split('_').collect();
    let (kind, rest): (Kind, &[&str]) = if t.first() == Some(&"p") {
        (anchor(t.get(1)?)?, &t[2..])
    } else {
        (component(t.first()?)?, &t[1..])
    };
    match ext {
        "ydd" | "yld" => {
            // jbib_000_u / p_head_000
            let number = digits(rest.first()?)?;
            let race = match (kind, rest.get(1)) {
                (Kind::Component(_), Some(r)) if r.chars().all(|c| c.is_ascii_alphabetic()) && r.len() <= 4 => r.to_string(),
                (_, None) => String::new(),
                _ => return None,
            };
            // jbib_007_u_1: an alternate model of drawable 007
            if rest.len() == 3 && ext == "ydd" {
                return Some(Role::Alternate { kind, number, alt: rest[2].parse().ok()? });
            }
            if rest.len() > 2 {
                return None;
            }
            Some(if ext == "ydd" { Role::Drawable { kind, number, race } } else { Role::Cloth { kind, number } })
        }
        // a texture is a .ytd, or an image that is made into one when the pack is built
        "ytd" | "png" | "jpg" | "jpeg" | "dds" | "tga" | "bmp" => {
            // jbib_diff_000_a_uni / p_head_diff_000_a
            if rest.first() != Some(&"diff") {
                return None;
            }
            let number = digits(rest.get(1)?)?;
            let letter = rest.get(2).filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_lowercase()))?.chars().next()?;
            let race = rest.get(3).map(|r| r.to_string()).unwrap_or_default();
            if rest.len() > 4 {
                return None;
            }
            Some(Role::Texture { kind, number, letter, race })
        }
        _ => None,
    }
}

/// An image or model file whose name starts like a clothing file (`jbib_`, `p_head_`, ...): only those are worth listing as ignored.
fn ext_is_clothing_like(p: &Path) -> bool {
    let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let n = n.rsplit('^').next().unwrap_or(&n);
    let first = n.split('_').next().unwrap_or("");
    first == "p" || COMPONENTS.contains(&first)
}

/// The sex a path suggests: a folder or file name with `female` / `mp_f_` (or `male` / `mp_m_`) in it.
pub fn sex_of_path(p: &Path) -> Option<Sex> {
    let s = p.to_string_lossy().to_lowercase();
    if s.contains("female") || s.contains("mp_f_") || s.contains("\\women") || s.contains("/women") {
        Some(Sex::Female)
    } else if s.contains("male") || s.contains("mp_m_") || s.contains("\\men\\") || s.contains("/men/") {
        Some(Sex::Male)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ScanItem {
    pub path: PathBuf,
    pub sex: Option<Sex>,
    pub role: Role,
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub items: Vec<ScanItem>,
    /// files that are not clothing file names
    pub ignored: Vec<PathBuf>,
}

/// Models picked on their own bring their textures and cloth: every `.ytd` / image / `.yld` in the same folder whose name
/// has the same component and number (`jbib_007_u.ydd` brings `jbib_diff_007_a_uni.ytd`, `_b_`, ...). Folders and other
/// files are passed through as they are.
pub fn with_textures(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = paths.to_vec();
    for p in paths {
        let Some(Role::Drawable { kind, number, .. }) = p.file_name().and_then(|n| parse_name(&n.to_string_lossy())) else { continue };
        let Some(dir) = p.parent() else { continue };
        let mut kids: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|q| q.is_file()).collect();
        kids.sort();
        for q in kids {
            let same = match q.file_name().and_then(|n| parse_name(&n.to_string_lossy())) {
                Some(Role::Texture { kind: k, number: n, .. }) | Some(Role::Cloth { kind: k, number: n }) | Some(Role::Alternate { kind: k, number: n, .. }) => k == kind && n == number,
                _ => false,
            };
            if same && !out.contains(&q) {
                out.push(q);
            }
        }
    }
    out
}

/// Open a built pack (a FiveM resource or its `stream` folder, names like `mp_f_freemode_01_mypack^jbib_000_u.ydd`) as a
/// project: every drawable at its number with its textures and cloth, the pack name from the files. Nothing is changed on disk;
/// the project points at the pack's files, and building writes a new resource elsewhere.
pub fn open_pack(dir: &Path) -> anyhow::Result<Project> {
    let scan = scan(&[dir.to_path_buf()]);
    let paired = pair(&scan, Sex::Male);
    if paired.drawables.is_empty() {
        anyhow::bail!("no clothing models found in {}", dir.display());
    }
    // the pack name sits between the ped (and `_p` for props) and the caret
    let name = paired.drawables.iter().find_map(|d| {
        let f = file_of(&d.ydd).to_lowercase();
        let (pre, _) = f.split_once('^')?;
        let rest = pre.strip_prefix("mp_m_freemode_01_").or_else(|| pre.strip_prefix("mp_f_freemode_01_"))?;
        Some(rest.strip_prefix("p_").unwrap_or(rest).to_string())
    });
    let mut p = Project { name: name.unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()), ..Default::default() };
    let plan = plan_import(&p, paired.drawables);
    for d in plan.fresh {
        p.insert_sorted(d);
    }
    // the same number twice (two addons of one pack): the second one goes after the last, nothing is lost
    for d in plan.conflicts {
        p.add_next(d);
    }
    Ok(p)
}

/// Look through files and folders (below folders too) for clothing files.
pub fn scan(paths: &[PathBuf]) -> Scan {
    fn walk(p: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if p.is_dir() {
            if depth > crate::search::MAX_FOLDER_DEPTH {
                return;
            }
            let mut kids: Vec<PathBuf> = fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
            kids.sort();
            for k in kids {
                walk(&k, out, depth + 1);
            }
        } else if p.is_file() {
            out.push(p.to_path_buf());
        }
    }
    let mut files = vec![];
    for p in paths {
        walk(p, &mut files, 0);
    }
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(f.clone()));
    let mut out = Scan::default();
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match parse_name(&name) {
            Some(role) => out.items.push(ScanItem { sex: sex_of_path(&f), path: f, role }),
            None => {
                // only mention files that look meant for the pack
                let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if matches!(ext.as_str(), "ydd" | "ytd" | "yld") || (matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "dds" | "tga" | "bmp") && ext_is_clothing_like(&f)) {
                    out.ignored.push(f);
                }
            }
        }
    }
    out
}

// ---- the project ------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub path: PathBuf,
    /// `a`..`z`, its index in the drawable
    pub letter: char,
    /// the part after the letter (`uni`, `whi`); empty = `uni`
    pub race: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawable {
    pub sex: Sex,
    pub kind: Kind,
    /// the drawable's index in the addon
    pub number: u32,
    /// the number its file had when imported, so a renumbering can be shown
    pub source_number: u32,
    pub ydd: PathBuf,
    pub race: String,
    pub cloth: Option<PathBuf>,
    /// alternate models (`jbib_007_u_1.ydd`): the same drawable as the game swaps it in some states (a jacket closed, hair
    /// under a hat), by their number
    pub alternates: Vec<(u32, PathBuf)>,
    pub textures: Vec<Texture>,
}

impl Drawable {
    pub fn label(&self) -> String {
        format!("{} {:03}", self.kind.name(), self.number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// the addon name: lowercase letters, digits and underscores
    pub name: String,
    /// how many drawables of one component and sex an addon may hold (128 on older game builds, 256 on newer ones)
    pub limit: usize,
    pub drawables: Vec<Drawable>,
    /// removed from the pack but kept (with their files untouched): they can be restored or moved to another pack, and are
    /// only forgotten when the user deletes them explicitly
    pub archived: Vec<Drawable>,
    /// build the streamed files into a folder per sex and category (`stream/female/jbib/`) instead of all in `stream/`
    pub sorted: bool,
}

impl Default for Project {
    fn default() -> Self {
        Self { name: "mypack".into(), limit: 128, drawables: vec![], archived: vec![], sorted: false }
    }
}

/// Files of one scan, paired up.
#[derive(Debug, Clone, Default)]
pub struct Paired {
    pub drawables: Vec<Drawable>,
    /// textures with no drawable of that number
    pub orphan_textures: Vec<ScanItem>,
    /// a second drawable file for the same sex, kind and number: it is in `drawables` too, with the same number, so the import
    /// asks where it goes
    pub duplicate_drawables: Vec<ScanItem>,
    extra: Vec<Drawable>,
}

/// Pair the drawables of a scan with their textures (same sex, kind and number). `default_sex` is used where the path says nothing.
pub fn pair(scan: &Scan, default_sex: Sex) -> Paired {
    let sex_of = |i: &ScanItem| i.sex.unwrap_or(default_sex);
    let mut out = Paired::default();
    let mut index: HashMap<(Sex, Kind, u32), usize> = HashMap::new();
    for it in &scan.items {
        if let Role::Drawable { kind, number, race } = &it.role {
            let key = (sex_of(it), *kind, *number);
            if index.contains_key(&key) {
                // the same number again (`lowr_035_u` and `lowr_035_r`): kept as a drawable of its own, which takes the same
                // textures; the import asks where it goes (the number is taken)
                out.duplicate_drawables.push(it.clone());
                out.extra.push(Drawable { sex: key.0, kind: *kind, number: *number, source_number: *number, ydd: it.path.clone(), race: race.clone(), cloth: None, alternates: vec![], textures: vec![] });
                continue;
            }
            index.insert(key, out.drawables.len());
            out.drawables.push(Drawable { sex: key.0, kind: *kind, number: *number, source_number: *number, ydd: it.path.clone(), race: race.clone(), cloth: None, alternates: vec![], textures: vec![] });
        }
    }
    for it in &scan.items {
        match &it.role {
            Role::Texture { kind, number, letter, race } => match index.get(&(sex_of(it), *kind, *number)) {
                Some(&i) => out.drawables[i].textures.push(Texture { path: it.path.clone(), letter: *letter, race: race.clone() }),
                None => out.orphan_textures.push(it.clone()),
            },
            Role::Cloth { kind, number } => {
                if let Some(&i) = index.get(&(sex_of(it), *kind, *number)) {
                    out.drawables[i].cloth = Some(it.path.clone());
                }
            }
            Role::Alternate { kind, number, alt } => match index.get(&(sex_of(it), *kind, *number)) {
                Some(&i) => out.drawables[i].alternates.push((*alt, it.path.clone())),
                None => out.orphan_textures.push(it.clone()),
            },
            Role::Drawable { .. } => {}
        }
    }
    // the extra models of a taken number get that number's textures and cloth too
    for e in &mut out.extra {
        if let Some(&i) = index.get(&(e.sex, e.kind, e.number)) {
            e.textures = out.drawables[i].textures.clone();
            e.cloth = out.drawables[i].cloth.clone();
        }
    }
    let extra = std::mem::take(&mut out.extra);
    out.drawables.extend(extra);
    for d in &mut out.drawables {
        d.alternates.sort();
        d.textures.sort_by_key(|t| t.letter);
    }
    out.drawables.sort_by_key(|d| (d.sex, d.kind, d.number));
    out
}

/// What to do with an incoming drawable whose number is already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// leave it out
    Skip,
    /// number it after the last drawable of its component (the user asked for this)
    Continue,
    /// give it back to the caller, who puts it in another addon
    SeparateAddon,
}

#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    /// no number clash: can be added as they are
    pub fresh: Vec<Drawable>,
    /// the number is taken by a drawable already in the project
    pub conflicts: Vec<Drawable>,
}

/// Sort incoming drawables into those that fit and those whose number is taken. Changes nothing.
pub fn plan_import(project: &Project, incoming: Vec<Drawable>) -> ImportPlan {
    let taken: HashSet<(Sex, Kind, u32)> = project.drawables.iter().map(|d| (d.sex, d.kind, d.number)).collect();
    let mut seen = taken.clone();
    let mut plan = ImportPlan::default();
    for d in incoming {
        if seen.insert((d.sex, d.kind, d.number)) {
            plan.fresh.push(d);
        } else {
            plan.conflicts.push(d);
        }
    }
    plan
}

fn next_number(drawables: &[Drawable], sex: Sex, kind: Kind) -> u32 {
    drawables.iter().filter(|d| d.sex == sex && d.kind == kind).map(|d| d.number + 1).max().unwrap_or(0)
}

/// Add the plan to the project; `policy` decides what happens to the conflicts. Returns the drawables meant for another addon.
/// The drawable's `source_number` keeps the number its file had, so the renumbering is visible.
pub fn apply_import(project: &mut Project, plan: ImportPlan, policy: OnConflict) -> Vec<Drawable> {
    project.drawables.extend(plan.fresh);
    let mut separate = vec![];
    for mut d in plan.conflicts {
        match policy {
            OnConflict::Skip => {}
            OnConflict::Continue => {
                d.number = next_number(&project.drawables, d.sex, d.kind);
                project.drawables.push(d);
            }
            OnConflict::SeparateAddon => separate.push(d),
        }
    }
    project.drawables.sort_by_key(|d| (d.sex, d.kind, d.number));
    separate
}

impl Project {
    /// Number each component's drawables `0, 1, 2, ...` in their current order, closing gaps. Only called when the user asks for it.
    pub fn close_gaps(&mut self) {
        let mut next: HashMap<(Sex, Kind), u32> = HashMap::new();
        self.drawables.sort_by_key(|d| (d.sex, d.kind, d.number));
        for d in &mut self.drawables {
            let n = next.entry((d.sex, d.kind)).or_insert(0);
            d.number = *n;
            *n += 1;
        }
    }

    /// Re-letter every drawable's textures `a`, `b`, `c`, ... in their current order.
    pub fn relabel_textures(&mut self) {
        for d in &mut self.drawables {
            for (i, t) in d.textures.iter_mut().enumerate() {
                t.letter = (b'a' + (i.min(25)) as u8) as char;
            }
        }
    }

    /// move a drawable one place earlier (`-1`) or later (`1`) among those of its component and sex, swapping numbers
    pub fn move_drawable(&mut self, index: usize, by: i32) {
        let Some(d) = self.drawables.get(index) else { return };
        let (sex, kind, number) = (d.sex, d.kind, d.number);
        let target = number as i64 + by as i64;
        if target < 0 {
            return;
        }
        let Some(other) = self.drawables.iter().position(|o| o.sex == sex && o.kind == kind && o.number as i64 == target) else { return };
        self.drawables[index].number = target as u32;
        self.drawables[other].number = number;
        self.drawables.sort_by_key(|d| (d.sex, d.kind, d.number));
    }

    /// Take drawable `index` out of the pack into the archive (its files are not touched). The ones after it keep their
    /// numbers: use Close the gaps to renumber.
    pub fn archive(&mut self, index: usize) -> Option<&Drawable> {
        let d = (index < self.drawables.len()).then(|| self.drawables.remove(index))?;
        self.archived.push(d);
        self.archived.last()
    }

    /// Put archived drawable `index` back, at the next free number of its sex and component. Returns its new number.
    pub fn restore(&mut self, index: usize) -> Option<u32> {
        let mut d = (index < self.archived.len()).then(|| self.archived.remove(index))?;
        d.number = next_number(&self.drawables, d.sex, d.kind);
        let n = d.number;
        self.insert_sorted(d);
        Some(n)
    }

    /// Add a drawable (from another pack, or the archive) at the next free number of its sex and component; returns the number.
    pub fn add_next(&mut self, mut d: Drawable) -> u32 {
        d.number = next_number(&self.drawables, d.sex, d.kind);
        let n = d.number;
        self.insert_sorted(d);
        n
    }

    /// Move drawable `index` to another slot: sex, component or prop, number. A drawable already there swaps into the
    /// moved one's old slot when it is of the same sex and kind, else it moves to the next free number of its own group,
    /// so nothing is overwritten. Textures and cloth go with their drawable (their names follow at build).
    pub fn move_to(&mut self, index: usize, sex: Sex, kind: Kind, number: u32) -> Result<(), String> {
        if index >= self.drawables.len() {
            return Err("no such drawable".into());
        }
        if kind.is_prop() != self.drawables[index].kind.is_prop() {
            return Err("a component cannot become a prop or a prop a component (they are different models in the game)".into());
        }
        let old = (self.drawables[index].sex, self.drawables[index].kind, self.drawables[index].number);
        if (sex, kind, number) == old {
            return Ok(());
        }
        if let Some(j) = self.drawables.iter().position(|d| (d.sex, d.kind, d.number) == (sex, kind, number)) {
            if (old.0, old.1) == (sex, kind) {
                self.drawables[j].number = old.2;
            } else {
                // the other one keeps its group and gets its first free number
                let free = next_number(&self.drawables, sex, kind);
                self.drawables[j].number = free;
            }
        }
        let d = &mut self.drawables[index];
        (d.sex, d.kind, d.number) = (sex, kind, number);
        // a drawable of another component has the other component's texture names too: the files are renamed at build
        let all = std::mem::take(&mut self.drawables);
        for d in all {
            self.insert_sorted(d);
        }
        Ok(())
    }

    /// keep the list in the order the page shows: sex, kind, number
    fn insert_sorted(&mut self, d: Drawable) {
        let at = self.drawables.iter().position(|x| (x.sex, x.kind, x.number) > (d.sex, d.kind, d.number)).unwrap_or(self.drawables.len());
        self.drawables.insert(at, d);
    }

    pub fn count(&self, sex: Sex, kind: Kind) -> usize {
        self.drawables.iter().filter(|d| d.sex == sex && d.kind == kind).count()
    }
}

// ---- checking ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sev {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub sev: Sev,
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// index into `Project::drawables`
    pub drawable: Option<usize>,
}

/// A hash of the file's content. Remembered per file with its size and modified time, so checking a project again (the
/// Clothing page does it whenever the project changes) does not read every texture again.
fn content_hash(p: &Path) -> Option<u64> {
    use std::sync::{Mutex, OnceLock};
    type Seen = HashMap<PathBuf, (u64, std::time::SystemTime, u64)>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let m = fs::metadata(p).ok()?;
    let stamp = (m.len(), m.modified().ok()?);
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&(len, time, h)) = seen.lock().ok()?.get(p) {
        if (len, time) == stamp {
            return Some(h);
        }
    }
    let bytes = fs::read(p).ok()?;
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let h = h.finish();
    if let Ok(mut s) = seen.lock() {
        if s.len() > 100_000 {
            s.clear();
        }
        s.insert(p.to_path_buf(), (stamp.0, stamp.1, h));
    }
    Some(h)
}

/// A valid addon name: lowercase letters, digits and underscores, starting with a letter.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("the name is empty".into());
    }
    if name.len() > 32 {
        return Some("the name is longer than 32 characters".into());
    }
    if !name.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false) {
        return Some("the name must start with a lowercase letter".into());
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')) {
        return Some(format!("'{c}' is not allowed: use lowercase letters, digits and underscores"));
    }
    None
}

/// Everything wrong or doubtful about a project, before anything is built.
pub fn validate(p: &Project) -> Vec<Issue> {
    let mut out: Vec<Issue> = vec![];
    let mut add = |sev, code, message: String, fix: &str, drawable: Option<usize>| out.push(Issue { sev, code, message, fix: fix.to_string(), drawable });
    if let Some(why) = name_problem(&p.name) {
        add(Sev::Error, "CLOTH_NAME", format!("the pack name {:?}: {why}", p.name), "FiveM and the game find the addon by this name; pick something like mypack", None);
    }
    if p.drawables.is_empty() {
        add(Sev::Error, "CLOTH_EMPTY", "the project has no drawables".into(), "drop .ydd files and their .ytd textures on the page", None);
    }
    // per (sex, kind): duplicate numbers, gaps, over the limit
    let mut groups: BTreeMap<(Sex, Kind), Vec<(usize, u32)>> = BTreeMap::new();
    for (i, d) in p.drawables.iter().enumerate() {
        groups.entry((d.sex, d.kind)).or_default().push((i, d.number));
    }
    for ((sex, kind), items) in &groups {
        let mut seen: HashMap<u32, usize> = HashMap::new();
        for (i, n) in items {
            if let Some(first) = seen.insert(*n, *i) {
                add(Sev::Error, "CLOTH_DUPLICATE_NUMBER", format!("{} {} {:03} is used twice ({} and {})", sex.label(), kind.name(), n, file_of(&p.drawables[first].ydd), file_of(&p.drawables[*i].ydd)), "give one of them another number (Continue numbering, or move it)", Some(*i));
            }
        }
        let mut nums: Vec<u32> = items.iter().map(|x| x.1).collect();
        nums.sort();
        nums.dedup();
        let gaps: Vec<u32> = (0..=nums.last().copied().unwrap_or(0)).filter(|n| nums.binary_search(n).is_err()).collect();
        if !gaps.is_empty() {
            add(Sev::Warning, "CLOTH_GAP", format!("{} {}: numbers {} are missing; the game numbers drawables by position, so a gap makes everything after it shift", sex.label(), kind.name(), gaps.iter().take(6).map(|n| format!("{n:03}")).collect::<Vec<_>>().join(", ")), "use Close the gaps (renumbers in order), or add the missing drawables", None);
        }
        if items.len() > p.limit {
            add(Sev::Warning, "CLOTH_LIMIT", format!("{} {} has {} drawables; one addon holds {} (the rest go to a second addon)", sex.label(), kind.name(), items.len(), p.limit), "the build splits the pack into name_01, name_02, ...; or raise the limit if your game build allows 256", None);
        }
    }
    for (i, d) in p.drawables.iter().enumerate() {
        let who = format!("{} {} {:03}", d.sex.label(), d.kind.name(), d.number);
        if !d.ydd.is_file() {
            add(Sev::Error, "CLOTH_MISSING_YDD", format!("{who}: the model file is missing: {}", d.ydd.display()), "re-add the .ydd, or remove the drawable", Some(i));
        }
        if let Some(c) = &d.cloth {
            if !c.is_file() {
                add(Sev::Warning, "CLOTH_MISSING_YLD", format!("{who}: the cloth file is missing: {}", c.display()), "re-add the .yld or remove the cloth from the drawable", Some(i));
            }
        }
        if d.textures.is_empty() {
            add(Sev::Error, "CLOTH_NO_TEXTURE", format!("{who} has no texture"), "every drawable needs at least one .ytd (jbib_diff_000_a_uni.ytd)", Some(i));
        }
        if d.textures.len() > MAX_TEXTURES {
            add(Sev::Error, "CLOTH_TOO_MANY_TEXTURES", format!("{who} has {} textures; the letters only go a..z ({MAX_TEXTURES})", d.textures.len()), "remove some textures", Some(i));
        }
        let mut letters: HashSet<char> = HashSet::new();
        let mut hashes: HashMap<u64, &Texture> = HashMap::new();
        // the same content has the same size: a file whose size no other texture of the drawable has is never read
        let sizes: Vec<Option<u64>> = d.textures.iter().map(|t| fs::metadata(&t.path).ok().filter(|m| m.is_file()).map(|m| m.len())).collect();
        for (k, t) in d.textures.iter().enumerate() {
            let Some(size) = sizes[k] else {
                add(Sev::Error, "CLOTH_MISSING_YTD", format!("{who}: texture {} is missing: {}", t.letter, t.path.display()), "re-add the .ytd, or remove the texture", Some(i));
                continue;
            };
            if !letters.insert(t.letter) {
                add(Sev::Error, "CLOTH_DUPLICATE_LETTER", format!("{who}: two textures are letter {}", t.letter), "Re-letter the textures (a, b, c, ...)", Some(i));
            }
            let size_shared = sizes.iter().filter(|s| **s == Some(size)).count() > 1;
            if let Some(h) = size_shared.then(|| content_hash(&t.path)).flatten() {
                if let Some(prev) = hashes.insert(h, t) {
                    add(Sev::Warning, "CLOTH_DUPLICATE_TEXTURE", format!("{who}: textures {} and {} are the same file content", prev.letter, t.letter), "remove one; it only costs size", Some(i));
                }
            }
        }
        let expected: Vec<char> = (0..d.textures.len().min(26)).map(|k| (b'a' + k as u8) as char).collect();
        let mut have: Vec<char> = d.textures.iter().map(|t| t.letter).collect();
        have.sort();
        if have != expected && have.iter().collect::<HashSet<_>>().len() == have.len() {
            add(Sev::Warning, "CLOTH_LETTER_GAP", format!("{who}: texture letters are {} (they should run a, b, c, ...)", have.iter().collect::<String>()), "Re-letter the textures", Some(i));
        }
    }
    out.sort_by_key(|i| (i.sev, i.drawable));
    out
}

pub fn file_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(n: &str) -> Option<Role> {
        parse_name(n)
    }

    #[test]
    fn file_names_are_read_the_way_the_game_writes_them() {
        assert_eq!(role("jbib_001_u.ydd"), Some(Role::Drawable { kind: Kind::Component(11), number: 1, race: "u".into() }));
        assert_eq!(role("JBIB_001.ydd"), Some(Role::Drawable { kind: Kind::Component(11), number: 1, race: String::new() }), "no race part, any case");
        assert_eq!(role("jbib_diff_001_a_uni.ytd"), Some(Role::Texture { kind: Kind::Component(11), number: 1, letter: 'a', race: "uni".into() }));
        assert_eq!(role("p_head_003.ydd"), Some(Role::Drawable { kind: Kind::Prop(0), number: 3, race: String::new() }));
        assert_eq!(role("p_eyes_diff_000_c.ytd"), Some(Role::Texture { kind: Kind::Prop(1), number: 0, letter: 'c', race: String::new() }));
        assert_eq!(role("lowr_012_r.yld"), Some(Role::Cloth { kind: Kind::Component(4) , number: 12 }));
        assert_eq!(role("mp_m_freemode_01_pack^uppr_002_u.ydd"), Some(Role::Drawable { kind: Kind::Component(3), number: 2, race: "u".into() }), "the FiveM caret name");
        for no in ["readme.txt", "jbib_u.ydd", "jbib_diff_001.ytd", "xxxx_001_u.ydd", "jbib_diff_001_ab_uni.ytd", "jbib_001_u_x.ydd", "p_head_diff_000_a_uni_x.ytd"] {
            assert_eq!(role(no), None, "{no}");
        }
        // names round trip
        assert_eq!(Kind::Component(11).drawable_file(7, ""), "jbib_007_u.ydd");
        assert_eq!(Kind::Component(11).texture_file(7, 'b', ""), "jbib_diff_007_b_uni.ytd");
        assert_eq!(Kind::Prop(0).drawable_file(7, "u"), "p_head_007.ydd");
        assert_eq!(Kind::Prop(0).texture_file(7, 'b', "uni"), "p_head_diff_007_b.ytd");
        assert_eq!(sex_of_path(Path::new("C:\\x\\female\\jbib_000_u.ydd")), Some(Sex::Female));
        assert_eq!(sex_of_path(Path::new("C:\\x\\male\\jbib_000_u.ydd")), Some(Sex::Male));
        assert_eq!(sex_of_path(Path::new("C:\\x\\jbib_000_u.ydd")), None);
    }

    #[test]
    #[ignore = "timing only: cargo test --release -p rage-toolkit-core validate_timing -- --ignored --nocapture"]
    fn validate_timing() {
        let d = temp("timing");
        let mut p = Project::default();
        for n in 0..100u32 {
            let textures = (0..3u8)
                .map(|k| {
                    let path = d.join(format!("jbib_diff_{n:03}_{}_uni.ytd", (b'a' + k) as char));
                    fs::write(&path, vec![n as u8 ^ k; 512 * 1024]).unwrap();
                    Texture { path, letter: (b'a' + k) as char, race: "uni".into() }
                })
                .collect();
            let ydd = d.join(format!("jbib_{n:03}_u.ydd"));
            fs::write(&ydd, b"m").unwrap();
            p.drawables.push(Drawable { sex: Sex::Male, kind: Kind::Component(11), number: n, source_number: n, ydd, race: "u".into(), cloth: None, alternates: vec![], textures });
        }
        for run in ["first", "again (unchanged files)"] {
            let t = std::time::Instant::now();
            let issues = validate(&p);
            println!("validate 100 drawables x 3 textures x 512 KB, {run}: {:.1} ms ({} issues)", t.elapsed().as_secs_f64() * 1000.0, issues.len());
        }
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn alternates_and_a_second_model_of_one_number_are_kept() {
        assert_eq!(parse_name("jbib_125_u_1.ydd"), Some(Role::Alternate { kind: Kind::Component(11), number: 125, alt: 1 }));
        assert_eq!(parse_name("mp_f_freemode_01_x^hair_000_u_1.ydd"), Some(Role::Alternate { kind: Kind::Component(2), number: 0, alt: 1 }));
        let d = temp("alt");
        for f in ["jbib_125_u.ydd", "jbib_125_u_1.ydd", "jbib_diff_125_a_uni.ytd", "lowr_035_u.ydd", "lowr_035_r.ydd", "lowr_diff_035_a_uni.ytd"] {
            put(&d, &format!("female/{f}"), "x");
        }
        let paired = pair(&scan(&[d.clone()]), Sex::Male);
        let jbib = paired.drawables.iter().find(|x| x.kind == Kind::Component(11)).unwrap();
        assert_eq!(jbib.alternates.iter().map(|(n, p)| (*n, file_of(p))).collect::<Vec<_>>(), [(1, "jbib_125_u_1.ydd".to_string())]);
        let lowr: Vec<&Drawable> = paired.drawables.iter().filter(|x| x.kind == Kind::Component(4)).collect();
        assert_eq!(lowr.len(), 2, "both lowr_035_u and lowr_035_r are kept");
        assert!(lowr.iter().all(|x| x.textures.len() == 1), "each with the number's texture");
        // the import asks for the second one (its number is taken)
        let plan = plan_import(&Project::default(), paired.drawables);
        assert_eq!((plan.fresh.len(), plan.conflicts.len()), (2, 1));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_model_picked_alone_brings_its_textures_and_cloth() {
        let d = temp("siblings");
        let ydd = put(&d, "jbib_007_u.ydd", "m");
        for f in ["jbib_diff_007_a_uni.ytd", "jbib_diff_007_b_uni.png", "jbib_007_u.yld", "jbib_diff_008_a_uni.ytd", "uppr_diff_007_a_uni.ytd", "notes.txt"] {
            put(&d, f, "x");
        }
        let got: Vec<String> = with_textures(std::slice::from_ref(&ydd)).iter().map(|p| file_of(p)).collect();
        assert_eq!(got, ["jbib_007_u.ydd", "jbib_007_u.yld", "jbib_diff_007_a_uni.ytd", "jbib_diff_007_b_uni.png"]);
        let paired = pair(&scan(&with_textures(&[ydd])), Sex::Male);
        assert_eq!(paired.drawables.len(), 1);
        assert_eq!(paired.drawables[0].textures.len(), 2);
        fs::remove_dir_all(d).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_cloth_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(d: &Path, name: &str, body: &str) -> PathBuf {
        let p = d.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_folder_is_scanned_and_models_are_paired_with_their_textures() {
        let d = temp("pair");
        put(&d, "male/jbib_001.ydd", "m1");
        put(&d, "male/jbib_diff_001_a_uni.ytd", "t1a");
        put(&d, "male/jbib_diff_001_b_uni.ytd", "t1b");
        put(&d, "male/jbib_diff_002_a_uni.ytd", "orphan");
        put(&d, "female/jbib_001_u.ydd", "f1");
        put(&d, "female/jbib_diff_001_a_uni.ytd", "ft");
        put(&d, "male/p_head_000.ydd", "hat");
        put(&d, "male/p_head_diff_000_a.ytd", "hatt");
        put(&d, "male/lowr_000_u.ydd", "l0");
        put(&d, "male/lowr_000_u.yld", "cloth");
        put(&d, "male/notes.txt", "x");
        put(&d, "male/weird_name.ydd", "x");
        let s = scan(&[d.clone()]);
        assert_eq!(s.ignored.len(), 1, "weird_name.ydd is mentioned, notes.txt is not");
        let p = pair(&s, Sex::Male);
        let male_jbib = p.drawables.iter().find(|x| x.sex == Sex::Male && x.kind == Kind::Component(11)).unwrap();
        assert_eq!((male_jbib.number, male_jbib.textures.len()), (1, 2), "jbib_001.ydd takes jbib_diff_001_a_uni and _b_uni");
        assert_eq!(male_jbib.textures.iter().map(|t| t.letter).collect::<String>(), "ab");
        assert!(p.drawables.iter().any(|x| x.sex == Sex::Female && x.kind == Kind::Component(11) && x.textures.len() == 1), "the female folder");
        assert!(p.drawables.iter().any(|x| x.kind == Kind::Prop(0) && x.textures.len() == 1));
        assert!(p.drawables.iter().find(|x| x.kind == Kind::Component(4)).unwrap().cloth.is_some());
        assert_eq!(p.orphan_textures.len(), 1, "jbib_diff_002 has no jbib_002.ydd");
        let _ = fs::remove_dir_all(&d);
    }

    fn drawable(sex: Sex, kind: Kind, number: u32, ydd: &Path, textures: &[(&Path, char)]) -> Drawable {
        Drawable { sex, kind, number, source_number: number, ydd: ydd.to_path_buf(), race: "u".into(), cloth: None, alternates: vec![], textures: textures.iter().map(|(p, l)| Texture { path: p.to_path_buf(), letter: *l, race: "uni".into() }).collect() }
    }

    #[test]
    fn importing_never_renumbers_silently_and_every_policy_is_explicit() {
        let d = temp("import");
        let (y, t) = (put(&d, "a.ydd", "y"), put(&d, "a.ytd", "t"));
        let mut project = Project::default();
        project.drawables.push(drawable(Sex::Male, Kind::Component(11), 0, &y, &[(&t, 'a')]));
        project.drawables.push(drawable(Sex::Male, Kind::Component(11), 1, &y, &[(&t, 'a')]));
        let incoming = vec![drawable(Sex::Male, Kind::Component(11), 1, &y, &[(&t, 'a')]), drawable(Sex::Male, Kind::Component(11), 5, &y, &[(&t, 'a')]), drawable(Sex::Female, Kind::Component(11), 0, &y, &[(&t, 'a')])];
        let plan = plan_import(&project, incoming.clone());
        assert_eq!((plan.fresh.len(), plan.conflicts.len()), (2, 1), "jbib 001 male clashes; jbib 005 male and female 000 do not");
        assert_eq!(project.drawables.len(), 2, "planning changes nothing");
        // skip
        let mut a = project.clone();
        assert!(apply_import(&mut a, plan_import(&project, incoming.clone()), OnConflict::Skip).is_empty());
        assert_eq!(a.drawables.len(), 4);
        // continue numbering: the clashing one gets the next number after the last (6, since 5 came in), and remembers its own
        let mut b = project.clone();
        apply_import(&mut b, plan_import(&project, incoming.clone()), OnConflict::Continue);
        assert_eq!(b.drawables.len(), 5);
        let moved = b.drawables.iter().find(|x| x.source_number == 1 && x.number != 1).expect("renumbered, and says so");
        assert_eq!(moved.number, 6);
        // a second addon: handed back, the project is untouched by it
        let mut c = project.clone();
        let sep = apply_import(&mut c, plan_import(&project, incoming), OnConflict::SeparateAddon);
        assert_eq!((sep.len(), c.drawables.len()), (1, 4));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn validation_finds_the_mistakes_that_only_show_up_in_game() {
        let d = temp("validate");
        let y = put(&d, "m.ydd", "y");
        let t1 = put(&d, "t1.ytd", "same");
        let t2 = put(&d, "t2.ytd", "same");
        let gone = d.join("gone.ytd");
        let mut p = Project { archived: vec![], sorted: false, name: "My Pack".into(), limit: 2, drawables: vec![] };
        let codes = |p: &Project| validate(p).iter().map(|i| i.code).collect::<Vec<_>>();
        assert!(codes(&p).contains(&"CLOTH_NAME") && codes(&p).contains(&"CLOTH_EMPTY"));
        p.name = "mypack".into();
        p.drawables.push(drawable(Sex::Male, Kind::Component(11), 0, &y, &[(&t1, 'a'), (&t2, 'b')])); // duplicate content
        p.drawables.push(drawable(Sex::Male, Kind::Component(11), 0, &y, &[(&t1, 'a')])); // duplicate number
        p.drawables.push(drawable(Sex::Male, Kind::Component(11), 3, &y, &[(&gone, 'a')])); // gap, missing texture
        p.drawables.push(drawable(Sex::Male, Kind::Component(4), 0, &y, &[])); // no texture
        let c = codes(&p);
        for expect in ["CLOTH_DUPLICATE_TEXTURE", "CLOTH_DUPLICATE_NUMBER", "CLOTH_GAP", "CLOTH_MISSING_YTD", "CLOTH_NO_TEXTURE", "CLOTH_LIMIT"] {
            assert!(c.contains(&expect), "{expect} missing from {c:?}");
        }
        assert!(validate(&p).iter().all(|i| !i.fix.is_empty() || i.sev == Sev::Info), "every finding says what to do");
        // a clean one
        let ok = Project { archived: vec![], sorted: false, name: "mypack".into(), limit: 128, drawables: vec![drawable(Sex::Male, Kind::Component(11), 0, &y, &[(&t1, 'a')]), drawable(Sex::Male, Kind::Component(11), 1, &y, &[(&t1, 'a'), (&t2, 'c')])] };
        let c = codes(&ok);
        assert!(!c.iter().any(|x| matches!(*x, "CLOTH_DUPLICATE_NUMBER" | "CLOTH_GAP" | "CLOTH_NO_TEXTURE")), "{c:?}");
        assert!(c.contains(&"CLOTH_LETTER_GAP"), "a, c is not a, b");
        // closing gaps and re-lettering are explicit actions and fix those two
        let mut fixed = p.clone();
        fixed.drawables.retain(|d| d.kind == Kind::Component(11));
        fixed.close_gaps();
        fixed.relabel_textures();
        assert_eq!(fixed.drawables.iter().map(|d| d.number).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(!codes(&fixed).contains(&"CLOTH_GAP"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn drawables_can_be_reordered() {
        let d = temp("order");
        let y = put(&d, "m.ydd", "y");
        let t = put(&d, "t.ytd", "t");
        let mut p = Project::default();
        for n in 0..3 {
            let mut dr = drawable(Sex::Male, Kind::Component(3), n, &y, &[(&t, 'a')]);
            dr.ydd = put(&d, &format!("u{n}.ydd"), "y");
            p.drawables.push(dr);
        }
        p.move_drawable(0, 1);
        assert_eq!(p.drawables.iter().map(|d| file_of(&d.ydd)).collect::<Vec<_>>(), ["u1.ydd", "u0.ydd", "u2.ydd"]);
        p.move_drawable(2, 1);
        assert_eq!(p.drawables[2].number, 2, "the last one cannot move later");
        let _ = fs::remove_dir_all(&d);
    }
}
