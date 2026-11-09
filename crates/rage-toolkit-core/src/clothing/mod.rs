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
pub mod extras;
pub mod game_dlc;
pub mod import;
pub mod vanilla_alts;
pub mod ymt;

/// The 12 ped components, in the game's order (the index is the `compIdx` of the YMT).
pub const COMPONENTS: [&str; 12] = ["head", "berd", "hair", "uppr", "lowr", "hand", "feet", "teef", "accs", "task", "decl", "jbib"];
/// The prop anchor points, in the game's order (`p_head_000` is anchor 0).
pub const ANCHORS: [&str; 13] = ["head", "eyes", "ears", "mouth", "lhand", "rhand", "lwrist", "rwrist", "hip", "lfoot", "rfoot", "ph_l_hand", "ph_r_hand"];
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
        (0..12u8).map(Kind::Component).chain((0..ANCHORS.len() as u8).map(Kind::Prop)).collect()
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
        // an anchor name can have underscores itself (`p_ph_l_hand_000`): the longest that fits
        let k = (1..=3).rev().find_map(|n| (t.len() > n).then(|| anchor(&t[1..=n].join("_")).map(|a| (a, n))).flatten())?;
        (k.0, &t[k.1 + 1..])
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
    let mut have: HashSet<PathBuf> = out.iter().cloned().collect();
    // each folder is read once, its textures, cloth and alternates indexed by (kind, number): picking thousands of models from
    // one folder must not read it thousands of times
    let mut folders: HashMap<PathBuf, HashMap<(Kind, u32), Vec<PathBuf>>> = HashMap::new();
    for p in paths {
        let Some(Role::Drawable { kind, number, .. }) = p.file_name().and_then(|n| parse_name(&n.to_string_lossy())) else { continue };
        let Some(dir) = p.parent() else { continue };
        let index = folders.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut kids: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).collect();
            kids.sort();
            let mut m: HashMap<(Kind, u32), Vec<PathBuf>> = HashMap::new();
            for q in kids {
                if let Some(Role::Texture { kind: k, number: n, .. } | Role::Cloth { kind: k, number: n } | Role::Alternate { kind: k, number: n, .. }) = q.file_name().and_then(|n| parse_name(&n.to_string_lossy())) {
                    m.entry((k, n)).or_default().push(q);
                }
            }
            m
        });
        for q in index.get(&(kind, number)).into_iter().flatten() {
            if have.insert(q.clone()) {
                out.push(q.clone());
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
    let name = name.unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    // the pack's files keep their DLC name
    let mut p = Project { name: name.clone(), dlcs: vec![Dlc { names: Some((name.clone(), name)), ..Default::default() }], ..Default::default() };
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Texture {
    pub path: PathBuf,
    /// `a`..`z`, its index in the drawable
    pub letter: char,
    /// the part after the letter (`uni`, `whi`); empty = `uni`
    pub race: String,
    /// the YMT's `texId` for this texture as read from a pack; `None` = the usual value (see [`Drawable::tex_id`])
    pub tex_id: Option<u8>,
    /// a change the user asked for on this texture (smaller, another format): applied to the built copy, the file stays as it is
    pub edit: Option<TexEdit>,
}

/// A texture made smaller or stored in another format in the build, set by the user for that texture. Nothing is enlarged; the
/// mip chain is made again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TexEdit {
    /// the longest side at most
    pub max: Option<u32>,
    /// the format to store it in (`dxt1`, `dxt5`, `bc7`, `ati2`, `a8r8g8b8`)
    pub format: Option<String>,
}

impl TexEdit {
    pub fn is_empty(&self) -> bool {
        self.max.is_none() && self.format.is_none()
    }

    /// this edit and another on top (a per-build size): the smaller size, this one's format first
    pub fn and(&self, other: &TexEdit) -> TexEdit {
        TexEdit { max: match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }, format: self.format.clone().or_else(|| other.format.clone()) }
    }
}

/// The formats a texture can be stored in, as the edit names them, with what each is for.
pub const TEX_FORMATS: [(&str, &str); 5] = [
    ("dxt1", "DXT1: no alpha (or cut-out), the smallest"),
    ("dxt5", "DXT5: smooth alpha"),
    ("bc7", "BC7: the best quality of the compressed formats"),
    ("ati2", "ATI2: two channels, for normal maps"),
    ("a8r8g8b8", "A8R8G8B8: uncompressed, four times DXT5's size"),
];

/// The component flags of a YMT (ePedCompFlags), by bit.
pub const COMP_FLAGS: [(u32, &str); 21] = [
    (1, "bulky"), (2, "job"), (4, "sunny"), (8, "wet"), (16, "cold"), (32, "not in car"), (64, "bike only"), (128, "not indoors"),
    (256, "fire retardant"), (512, "armoured"), (1024, "lightly armoured"), (2048, "high detail"), (4096, "default helmet"),
    (8192, "random helmet"), (16384, "script helmet"), (32768, "flight helmet"), (65536, "hide in first person"),
    (131072, "use physics hat 2"), (262144, "pilot helmet"), (524288, "wet: more wet"), (1048576, "wet: less wet"),
];

/// The YMT settings of one drawable beyond its files: what the game does with it (high heels, hair under a hat, flags, sounds).
/// Everything read from a pack's YMT is kept here, also what the page does not show, so a pack rebuilds as it was.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DrawableMeta {
    /// the YMT's `propMask`; `None` = the usual value (17 for a `_r` drawable, else 1). Packs use other bits too (5, 11, 65).
    pub prop_mask: Option<u8>,
    /// `flags` of a component (ePedCompFlags: bulky, hide in first person, ...) or `propFlags` of a prop
    pub flags: u32,
    /// the audio id (`pedXml_audioID` / `audioId`): a name or `hash_XXXXXXXX`, empty for none; `None` = the usual value
    /// (`none` for a component, empty for a prop)
    pub audio: Option<String>,
    /// `pedXml_audioID2` of a component, like `audio`; `None` = empty
    pub audio2: Option<String>,
    /// the five expression values: f4 is the high-heel height of feet, f0 the hair squash (negative) of a hat
    pub expression_mods: [f32; 5],
    /// `pedXml_flags` of a component, `flags` of a prop (the other flag word: `flags` above is `propFlags` there)
    pub flags2: u32,
    /// a prop's render flags (bits of ePropRenderFlags: alpha, decal, cutout)
    pub render_flags: u32,
    /// a prop's `stickyness`
    pub stickyness: u8,
    /// a mask that hides the hair under it: the build writes the hair switches into pedalternatevariations
    pub hides_hair: bool,
    /// the alternate model (`_1`) the game shows in first person, written to first_person_alternates
    pub first_person: Option<u32>,
}

impl DrawableMeta {
    /// the high-heel height (feet only), when set
    pub fn heels(&self) -> Option<f32> {
        (self.expression_mods[4] != 0.0).then_some(self.expression_mods[4])
    }

    /// how much a hat squashes the hair (a positive number, 0..1), when set
    pub fn hair_scale(&self) -> Option<f32> {
        (self.expression_mods[0] != 0.0).then_some(-self.expression_mods[0])
    }
}

/// A new id for a drawable, unique in this run and (seeded from the clock) against ids saved by earlier runs.
pub fn new_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let _ = NEXT.compare_exchange(0, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1, Ordering::SeqCst, Ordering::SeqCst);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawable {
    /// stable across renumbering and saving: references to a drawable (alternate variations) hold this, never its number
    pub id: u64,
    /// which DLC of the project it is in (index into `Project::dlcs`)
    pub dlc: usize,
    pub sex: Sex,
    pub kind: Kind,
    /// the drawable's index in its DLC
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
    pub meta: DrawableMeta,
    /// changes the user asked for on textures embedded in the model, by texture name (applied to the built copies of the model
    /// and its alternates)
    pub model_edits: Vec<(String, TexEdit)>,
}

impl Default for Drawable {
    fn default() -> Self {
        Self { id: new_id(), dlc: 0, sex: Sex::Male, kind: Kind::Component(0), number: 0, source_number: 0, ydd: PathBuf::new(), race: String::new(), cloth: None, alternates: vec![], textures: vec![], meta: DrawableMeta::default(), model_edits: vec![] }
    }
}

impl Drawable {
    pub fn label(&self) -> String {
        format!("{} {:03}", self.kind.name(), self.number)
    }

    /// a `_r` drawable: its textures carry the skin tone (`_whi`, ...) and its YMT entry says so
    pub fn is_race(&self) -> bool {
        self.race == "r"
    }

    /// the YMT's `propMask`
    pub fn prop_mask(&self) -> u8 {
        self.meta.prop_mask.unwrap_or(if self.is_race() { 17 } else { 1 })
    }

    /// the YMT's `texId` of texture `n`: a component's is 1 on a `_r` drawable, else 0; a prop's is its index
    pub fn tex_id(&self, n: usize) -> u8 {
        self.textures.get(n).and_then(|t| t.tex_id).unwrap_or(match self.kind {
            Kind::Prop(_) => n as u8,
            Kind::Component(_) => u8::from(self.is_race()),
        })
    }

    /// (DLC, sex, kind): the group whose numbers run 0, 1, 2, ...
    pub fn group(&self) -> (usize, Sex, Kind) {
        (self.dlc, self.sex, self.kind)
    }

    /// (DLC, sex, kind, number): its place in the game
    pub fn slot(&self) -> Slot {
        (self.dlc, self.sex, self.kind, self.number)
    }
}

/// DLC, sex, kind, number
pub type Slot = (usize, Sex, Kind, u32);

/// One DLC of the project: one YMT and one shop meta per sex. The game finds drawables by DLC name and position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dlc {
    /// the DLC names as they are (male, female), for a DLC that came from an existing pack; `None` = `<project>_<n>`
    pub names: Option<(String, String)>,
    /// the shop meta's `creatureMetaData` (high heels, hats) for an imported DLC; `None` = `MP_CreatureMetadata_<dlc>`
    pub creature: Option<String>,
    /// one of Rockstar's DLCs (`mp_m_2023_02`): its YMT is replaced and its drawables streamed over or after the game's,
    /// with no shop meta of its own
    pub base: bool,
    /// a Rockstar DLC switched off in the build: nothing of it is written (its YMT, files, creature metadata, alternate
    /// variations, first-person files), so the game's own stays
    pub left_out: bool,
    /// a Rockstar DLC added from the game: the entries of the game's creature metadata, kept exactly; the build adds entries only
    /// for drawables of the project and writes the file only when it has any (`None`: generated, as for our own DLCs)
    pub game_creature: Option<Vec<extras::Expression>>,
}

/// A drawable in an alternate variation: one of ours (followed through renumbering), or one elsewhere (the game's, another pack's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef {
    Ours(u64),
    /// `dlc` as the file writes it (empty for the base game), component, index
    Other { dlc: String, component: u8, index: u32 },
}

/// One switch of pedalternatevariations.meta: while any of `sources` is worn, `target` shows its alternate model `alt`
/// (a hand under a jacket, hair under a mask). Read from a pack and written back; ours follow their drawables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltSwitch {
    /// the DLC whose pedalternatevariations file holds it (packs keep one file per DLC, and the same target can be in several)
    pub file: usize,
    pub sex: Sex,
    pub target: AssetRef,
    pub alt: u32,
    pub sources: Vec<AssetRef>,
}

/// Shrinking textures while the pack is built: the sources are never changed, the build writes smaller copies. A texture is only
/// ever made smaller (Lanczos, a full mip chain, its format kept); files with nothing over the size are copied as they are.
///
/// Off unless asked for, every time: it is set for one build (the build window, `clothing build --ytd-max`) and never saved
/// with the project, so no build shrinks textures because an earlier one did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextureOptimize {
    /// the longest side of a texture in a `.ytd` (`None`: `.ytd` files are copied)
    pub ytd_max: Option<u32>,
    /// the longest side of a texture embedded in a model (`.ydd`); `None`: models are copied
    pub ydd_max: Option<u32>,
    /// only embedded textures of these formats (`ati2`, `dxt5`, ... comma separated); empty for all
    pub ydd_formats: String,
}

impl TextureOptimize {
    pub fn is_on(&self) -> bool {
        self.ytd_max.is_some() || self.ydd_max.is_some()
    }

    /// what the settings are, as one line (an update rebuilds every optimized file when it changes)
    pub fn stamp(&self) -> String {
        format!("ytd {:?} ydd {:?} {}", self.ytd_max, self.ydd_max, self.ydd_formats.to_lowercase())
    }
}

/// How a build lays the pack out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// one FiveM resource named after the project, with everything in it
    #[default]
    Single,
    /// a control resource (`<project>_control`: manifest, metas, YMTs, creature metadata) and stream resources with only the
    /// files: `<project>_stream_m1`, `_f1`, ... (sex and DLC number), each with at most `stream_limit` drawables of a type, the
    /// rest in `_m1_02`, `_m1_03`, ...; Rockstar DLCs in `<project>_stream_base`
    ControlAndStreams,
}

/// Where a carried file goes when the pack is built (in one resource: all into it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// with the YMTs and metas (the control resource)
    Control,
    /// with the streamed replacements of Rockstar's DLCs (the base stream resource)
    BaseStream,
    /// the stream resource it came from, by its name after the project's (`stream_f1` of `<project>_stream_f1`)
    Stream(String),
}

/// A file from an opened pack that the project does not model, written into the build as it is: a YMT that only blanks a
/// Rockstar DLC, loose replacements of Rockstar's files, a data file of a type the builder does not make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub place: Place,
    /// its path inside the resource (`stream/x.ymt`, `data/y.meta`), with `/`
    pub dest: String,
    pub source: PathBuf,
    /// the `data_file` type the manifest registers it as, if any
    pub data_file: Option<String>,
}

/// A first-person entry read from a pack that is not one of the project's drawables (another pack's, or a name that matches
/// nothing, like a female asset under a male DLC name): written back as it was, into the file of DLC `file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherFirstPerson {
    pub file: usize,
    /// `MP_M_Freemode_01_<dlc>/<drawable>` as the file has it
    pub asset: String,
    pub alt: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// the project name: lowercase letters, digits and underscores. The resources are named after it
    /// (`<name>_control`, `<name>_stream_m1`) and so are its new DLCs (`<name>_1`, `<name>_2`).
    pub name: String,
    /// how many drawables of one component and sex a DLC may hold (128 on older game builds, 256 on newer ones)
    pub limit: usize,
    /// how many drawables of one type and sex one stream resource holds (a bigger DLC is streamed by several)
    pub stream_limit: usize,
    /// the DLCs; every drawable's `dlc` is an index into this, and there is always at least one
    pub dlcs: Vec<Dlc>,
    pub drawables: Vec<Drawable>,
    /// removed from the pack but kept (with their files untouched): they can be restored or moved to another pack, and are
    /// only forgotten when the user deletes them explicitly
    pub archived: Vec<Drawable>,
    /// build the streamed files into a folder per sex and category (`stream/female/jbib/`) instead of all in `stream/`
    pub sorted: bool,
    /// alternate variations read from a pack, written back at build
    pub alt_switches: Vec<AltSwitch>,
    /// first-person entries read from a pack that point at no drawable of the project
    pub other_first_person: Vec<OtherFirstPerson>,
    /// files from an opened pack written into the build as they are
    pub carried: Vec<Carried>,
    pub layout: Layout,
    /// textures shrunk while building: only for the build it is set for, never saved (see [`TextureOptimize`])
    pub optimize: TextureOptimize,
}

impl Default for Project {
    fn default() -> Self {
        Self { name: "mypack".into(), limit: 256, stream_limit: 128, dlcs: vec![Dlc::default()], drawables: vec![], archived: vec![], sorted: false, alt_switches: vec![], other_first_person: vec![], carried: vec![], layout: Layout::Single, optimize: TextureOptimize::default() }
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
                out.extra.push(Drawable { sex: key.0, kind: *kind, number: *number, source_number: *number, ydd: it.path.clone(), race: race.clone(), ..Default::default() });
                continue;
            }
            index.insert(key, out.drawables.len());
            out.drawables.push(Drawable { sex: key.0, kind: *kind, number: *number, source_number: *number, ydd: it.path.clone(), race: race.clone(), ..Default::default() });
        }
    }
    for it in &scan.items {
        match &it.role {
            Role::Texture { kind, number, letter, race } => match index.get(&(sex_of(it), *kind, *number)) {
                Some(&i) => out.drawables[i].textures.push(Texture { path: it.path.clone(), letter: *letter, race: race.clone(), tex_id: None, edit: None }),
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

/// Sort incoming drawables into those that fit and those whose number is taken. Changes nothing. Incoming drawables keep
/// their `dlc`: drawables from files land in the first DLC (0).
pub fn plan_import(project: &Project, incoming: Vec<Drawable>) -> ImportPlan {
    let mut seen: HashSet<Slot> = project.drawables.iter().map(Drawable::slot).collect();
    let mut plan = ImportPlan::default();
    for d in incoming {
        if seen.insert(d.slot()) {
            plan.fresh.push(d);
        } else {
            plan.conflicts.push(d);
        }
    }
    plan
}

fn next_number(drawables: &[Drawable], group: (usize, Sex, Kind)) -> u32 {
    drawables.iter().filter(|d| d.group() == group).map(|d| d.number + 1).max().unwrap_or(0)
}

/// Add the plan to the project; `policy` decides what happens to the conflicts. Returns the drawables meant for another addon.
/// The drawable's `source_number` keeps the number its file had, so the renumbering is visible.
pub fn apply_import(project: &mut Project, plan: ImportPlan, policy: OnConflict) -> Vec<Drawable> {
    for d in plan.fresh {
        project.ensure_dlc(d.dlc);
        project.drawables.push(d);
    }
    let mut separate = vec![];
    for d in plan.conflicts {
        match policy {
            OnConflict::Skip => {}
            OnConflict::Continue => {
                project.add_next(d);
            }
            OnConflict::SeparateAddon => separate.push(d),
        }
    }
    project.drawables.sort_by_key(Drawable::slot);
    separate
}

impl Project {
    /// The DLC name of DLC `dlc` for one sex: as imported, or `<project>_<n>` (n counting from 1).
    pub fn dlc_name(&self, dlc: usize, sex: Sex) -> String {
        match self.dlcs.get(dlc).and_then(|d| d.names.as_ref()) {
            Some((m, f)) => if sex == Sex::Male { m.clone() } else { f.clone() },
            None => format!("{}_{}", self.name, dlc + 1),
        }
    }

    /// The name of DLC `dlc` without the sex: the imported male name with its `mp_m_` taken off, or `<project>_<n>`.
    pub fn dlc_label(&self, dlc: usize) -> String {
        let m = self.dlc_name(dlc, Sex::Male);
        m.strip_prefix("mp_m_").map(str::to_string).unwrap_or(m)
    }

    /// The creature metadata (high heels, hats) the shop meta of DLC `dlc` names.
    pub fn creature_name(&self, dlc: usize) -> String {
        self.dlcs.get(dlc).and_then(|d| d.creature.clone()).unwrap_or_else(|| format!("MP_CreatureMetadata_{}", self.dlc_label(dlc)))
    }

    /// make sure DLC `dlc` exists (new ones are named after the project)
    pub fn ensure_dlc(&mut self, dlc: usize) {
        while self.dlcs.len() <= dlc {
            self.dlcs.push(Dlc::default());
        }
    }

    /// The next free slot of a sex and kind, from DLC `from` on: after the last drawable of that DLC, or in the next DLC when
    /// that one is full (`limit`). A DLC is added when all are full.
    pub fn next_slot(&mut self, from: usize, sex: Sex, kind: Kind) -> (usize, u32) {
        let mut dlc = from;
        loop {
            self.ensure_dlc(dlc);
            let n = next_number(&self.drawables, (dlc, sex, kind));
            // a Rockstar DLC is only added to on purpose (the user moves a drawable there)
            if (n as usize) < self.limit.max(1) && !(self.dlcs[dlc].base && dlc != from) {
                return (dlc, n);
            }
            dlc += 1;
        }
    }

    /// Number each component's drawables `0, 1, 2, ...` in their current order, closing gaps. Only called when the user asks for it.
    pub fn close_gaps(&mut self) {
        let mut next: HashMap<(usize, Sex, Kind), u32> = HashMap::new();
        self.drawables.sort_by_key(Drawable::slot);
        for d in &mut self.drawables {
            let n = next.entry(d.group()).or_insert(0);
            d.number = *n;
            *n += 1;
        }
    }

    /// Move the drawables over a DLC's `limit` into the next DLCs (numbered after their last), so every DLC fits. Only on request.
    /// Returns how many moved.
    pub fn split_full_dlcs(&mut self) -> usize {
        let limit = self.limit.max(1) as u32;
        let dlcs = self.dlcs.clone();
        let (mut moved, keep): (Vec<Drawable>, Vec<Drawable>) = std::mem::take(&mut self.drawables).into_iter().partition(|d| d.number >= limit && !dlcs.get(d.dlc).is_some_and(|x| x.base));
        self.drawables = keep;
        moved.sort_by_key(Drawable::slot);
        let n = moved.len();
        for d in moved {
            let from = d.dlc + 1;
            let (dlc, number) = self.next_slot(from, d.sex, d.kind);
            self.insert_sorted(Drawable { dlc, number, ..d });
        }
        n
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
        let (group, number) = (d.group(), d.number);
        let target = number as i64 + by as i64;
        if target < 0 {
            return;
        }
        let Some(other) = self.drawables.iter().position(|o| o.group() == group && o.number as i64 == target) else { return };
        self.drawables[index].number = target as u32;
        self.drawables[other].number = number;
        self.drawables.sort_by_key(Drawable::slot);
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
        let d = (index < self.archived.len()).then(|| self.archived.remove(index))?;
        Some(self.add_next(d))
    }

    /// Add a drawable (from another pack, or the archive) at the next free number of its sex and component, in its DLC or
    /// the next one with room; returns the number.
    pub fn add_next(&mut self, mut d: Drawable) -> u32 {
        (d.dlc, d.number) = self.next_slot(d.dlc, d.sex, d.kind);
        let n = d.number;
        self.insert_sorted(d);
        n
    }

    /// Move drawable `index` to another slot: DLC, sex, component or prop, number. A drawable already there swaps into the
    /// moved one's old slot when it is of the same DLC, sex and kind, else it moves to the next free number of its own group,
    /// so nothing is overwritten. Textures and cloth go with their drawable (their names follow at build).
    pub fn move_to(&mut self, index: usize, to: Slot) -> Result<(), String> {
        let (dlc, sex, kind, _) = to;
        if index >= self.drawables.len() {
            return Err("no such drawable".into());
        }
        if kind.is_prop() != self.drawables[index].kind.is_prop() {
            return Err("a component cannot become a prop or a prop a component (they are different models in the game)".into());
        }
        let old = self.drawables[index].slot();
        if to == old {
            return Ok(());
        }
        self.ensure_dlc(dlc);
        if let Some(j) = self.drawables.iter().position(|d| d.slot() == to) {
            if (old.0, old.1, old.2) == (dlc, sex, kind) {
                self.drawables[j].number = old.3;
            } else {
                // the other one keeps its group and gets its first free number
                let free = next_number(&self.drawables, (dlc, sex, kind));
                self.drawables[j].number = free;
            }
        }
        let d = &mut self.drawables[index];
        (d.dlc, d.sex, d.kind, d.number) = to;
        // a drawable of another component has the other component's texture names too: the files are renamed at build
        let all = std::mem::take(&mut self.drawables);
        for d in all {
            self.insert_sorted(d);
        }
        Ok(())
    }

    /// keep the list in the order the page shows: sex, kind, number
    fn insert_sorted(&mut self, d: Drawable) {
        self.ensure_dlc(d.dlc);
        let at = self.drawables.iter().position(|x| x.slot() > d.slot()).unwrap_or(self.drawables.len());
        self.drawables.insert(at, d);
    }

    /// how many drawables of a sex and kind the project has, over all DLCs
    pub fn count(&self, sex: Sex, kind: Kind) -> usize {
        self.drawables.iter().filter(|d| d.sex == sex && d.kind == kind).count()
    }

    pub fn find(&self, id: u64) -> Option<&Drawable> {
        self.drawables.iter().find(|d| d.id == id)
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
    // the DLC part of a message, only when there is more than one
    let in_dlc = |dlc: usize| if p.dlcs.len() > 1 { format!("{} ", p.dlc_label(dlc)) } else { String::new() };
    let mut names: HashMap<String, usize> = HashMap::new();
    for k in 0..p.dlcs.len() {
        for sex in [Sex::Male, Sex::Female] {
            let n = p.dlc_name(k, sex).to_lowercase();
            if let Some(name_problem) = name_problem(&n).filter(|_| p.dlcs[k].names.is_some()) {
                add(Sev::Warning, "CLOTH_DLC_NAME", format!("DLC {n}: {name_problem}"), "it came from a pack this way; FiveM may still load it", None);
            }
            if let Some(other) = names.insert(n.clone(), k).filter(|o| *o != k) {
                add(Sev::Error, "CLOTH_DLC_SAME_NAME", format!("DLCs {} and {} are both named {n}", other + 1, k + 1), "rename one of them; the game would see one DLC", None);
            }
        }
    }
    // per (DLC, sex, kind): duplicate numbers, gaps, over the limit
    let mut groups: BTreeMap<(usize, Sex, Kind), Vec<(usize, u32)>> = BTreeMap::new();
    for (i, d) in p.drawables.iter().enumerate() {
        if d.dlc >= p.dlcs.len() {
            add(Sev::Error, "CLOTH_NO_DLC", format!("{} {:03} is in DLC {}, which the project does not have", d.kind.name(), d.number, d.dlc + 1), "move it to one of the project's DLCs", Some(i));
        }
        groups.entry(d.group()).or_default().push((i, d.number));
    }
    for ((dlc, sex, kind), items) in &groups {
        let at = in_dlc(*dlc);
        let mut seen: HashMap<u32, usize> = HashMap::new();
        for (i, n) in items {
            if let Some(first) = seen.insert(*n, *i) {
                add(Sev::Error, "CLOTH_DUPLICATE_NUMBER", format!("{at}{} {} {:03} is used twice ({} and {})", sex.label(), kind.name(), n, file_of(&p.drawables[first].ydd), file_of(&p.drawables[*i].ydd)), "give one of them another number (Continue numbering, or move it)", Some(*i));
            }
        }
        let mut nums: Vec<u32> = items.iter().map(|x| x.1).collect();
        nums.sort();
        nums.dedup();
        let gaps: Vec<u32> = (0..=nums.last().copied().unwrap_or(0)).filter(|n| nums.binary_search(n).is_err()).collect();
        if !gaps.is_empty() {
            add(Sev::Warning, "CLOTH_GAP", format!("{at}{} {}: numbers {} are missing; the game numbers drawables by position, so a gap makes everything after it shift", sex.label(), kind.name(), gaps.iter().take(6).map(|n| format!("{n:03}")).collect::<Vec<_>>().join(", ")), "use Close the gaps (renumbers in order), or add the missing drawables", None);
        }
        if nums.last().is_some_and(|n| *n as usize >= p.limit) && !p.dlcs.get(*dlc).is_some_and(|x| x.base) {
            add(Sev::Error, "CLOTH_LIMIT", format!("{at}{} {} has {} drawables; one DLC holds {}", sex.label(), kind.name(), items.len(), p.limit), "press Move overflow to next DLC, or raise the limit (256 needs a recent game build)", None);
        }
    }
    for (i, d) in p.drawables.iter().enumerate() {
        let who = format!("{}{} {} {:03}", in_dlc(d.dlc), d.sex.label(), d.kind.name(), d.number);
        if d.meta.heels().is_some() && d.kind != Kind::Component(6) {
            add(Sev::Warning, "CLOTH_HEELS_NOT_FEET", format!("{who} has a high-heel height, which only feet use"), "clear it, or move the drawable to feet", Some(i));
        }
        if d.meta.hair_scale().is_some() && d.kind != Kind::Prop(0) {
            add(Sev::Warning, "CLOTH_HAIR_SCALE_NOT_HAT", format!("{who} squashes the hair, which only hats (p_head) do"), "clear it, or move the drawable to p_head", Some(i));
        }
        if d.meta.hides_hair && d.kind != Kind::Component(1) {
            add(Sev::Warning, "CLOTH_HIDES_HAIR_NOT_MASK", format!("{who} is marked as hiding the hair, which only masks (berd) do"), "clear it; the hair switches are only written for masks", Some(i));
        }
        if let Some(n) = d.meta.first_person.filter(|n| !d.alternates.iter().any(|(a, _)| a == n)) {
            add(Sev::Error, "CLOTH_FIRST_PERSON_MISSING", format!("{who} shows alternate _{n} in first person, but has no such alternate model"), "add the _{n} model, or turn the first-person model off", Some(i));
        }
        // An empty path is a file the pack does not have: in a Rockstar DLC the game's own (not replaced), in an addon a slot an
        // opened pack had without its file (the game shows nothing there). The build writes the YMT as it is and no file.
        let games = p.dlcs.get(d.dlc).is_some_and(|x| x.base);
        if d.ydd.as_os_str().is_empty() {
            if !games {
                add(Sev::Warning, "CLOTH_MODEL_NOT_IN_PACK", format!("{who}: the YMT has this slot but the pack never had its model"), "add the .ydd, or remove the drawable (the numbers after it shift)", Some(i));
            }
        } else if !d.ydd.is_file() {
            add(Sev::Error, "CLOTH_MISSING_YDD", format!("{who}: the model file is missing: {}", d.ydd.display()), "re-add the .ydd, or remove the drawable", Some(i));
        }
        for (n, a) in d.alternates.iter().filter(|(_, a)| a.as_os_str().is_empty()) {
            if !games {
                add(Sev::Warning, "CLOTH_ALTERNATE_NOT_IN_PACK", format!("{who}: the YMT counts alternate model _{n} but the pack never had it"), "add the _{n} model, or remove the alternate", Some(i));
            }
            let _ = a;
        }
        if let Some(c) = d.cloth.as_ref().filter(|c| !c.as_os_str().is_empty()) {
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
            if t.path.as_os_str().is_empty() {
                if !games {
                    add(Sev::Warning, "CLOTH_TEXTURE_NOT_IN_PACK", format!("{who}: texture {} is in the YMT but the pack never had it (the game shows a missing texture)", t.letter), "add the texture, or remove it from the drawable", Some(i));
                }
                continue;
            }
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

/// Any metadata file (a binary YMT, a creature-metadata RBF, a PSO or XML) as the XML text tools show it, with the reader's
/// warnings. Used to look inside a pack's YMTs and to compare a rebuilt pack with the original.
pub fn metadata_xml(data: &[u8]) -> anyhow::Result<(String, Vec<String>)> {
    let (_, dump) = rage_formats::ymf::dump_metadata(data)?;
    Ok((rage_formats::to_xml(&dump.root, rage_formats::NameTable::core_shared()), dump.warnings))
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
        assert_eq!(role("p_ph_l_hand_002.ydd"), Some(Role::Drawable { kind: Kind::Prop(11), number: 2, race: String::new() }), "an anchor with underscores");
        assert_eq!(role("p_lfoot_diff_001_b.ytd"), Some(Role::Texture { kind: Kind::Prop(9), number: 1, letter: 'b', race: String::new() }));
        assert_eq!(Kind::Prop(12).drawable_file(4, ""), "p_ph_r_hand_004.ydd");
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
                    Texture { path, letter: (b'a' + k) as char, race: "uni".into(), tex_id: None, edit: None }
                })
                .collect();
            let ydd = d.join(format!("jbib_{n:03}_u.ydd"));
            fs::write(&ydd, b"m").unwrap();
            p.drawables.push(Drawable { sex: Sex::Male, kind: Kind::Component(11), number: n, source_number: n, ydd, race: "u".into(), textures, ..Default::default() });
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
        Drawable { sex, kind, number, source_number: number, ydd: ydd.to_path_buf(), race: "u".into(), textures: textures.iter().map(|(p, l)| Texture { path: p.to_path_buf(), letter: *l, race: "uni".into(), tex_id: None, edit: None }).collect(), ..Default::default() }
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
        let mut p = Project { name: "My Pack".into(), limit: 2, ..Default::default() };
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
        let ok = Project { name: "mypack".into(), limit: 128, ..Default::default() };
        let ok = Project { drawables: vec![drawable(Sex::Male, Kind::Component(11), 0, &y, &[(&t1, 'a')]), drawable(Sex::Male, Kind::Component(11), 1, &y, &[(&t1, 'a'), (&t2, 'c')])], ..ok };
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
