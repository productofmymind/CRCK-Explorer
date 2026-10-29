//! What the Explorer shows: the installed game's folder (its archives open in place, nested archives like folders) and any
//! folders the user adds. A `Node` is a folder, an archive or a file; folders and archives load their list when first looked at.
use anyhow::{bail, Context, Result};
use rage_toolkit_core::game::{cache_dir, Archive, Game};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Clone)]
pub enum Loc {
    Disk(PathBuf),
    /// inside an opened archive; `path` is the directory (with trailing '/') or file inside it, `origin` the chain of archives
    Arch { ar: Rc<Archive>, path: String, origin: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dir,
    Rpf,
    File,
}

pub struct Node {
    pub name: String,
    pub kind: Kind,
    pub loc: Loc,
    pub size: u64,
    children: Option<Vec<Node>>,
    error: Option<String>,
    /// a folder the user added: it can be taken out of the list again
    pub removable: bool,
}

/// What the Explorer asks the window to do.
pub enum Action {
    Open(Loc, String),
    AddTextures(Loc, String),
    Extract(Loc, String, Kind),
    /// a model written as XML: asks where to save it
    ExportXml(Loc, String),
    /// a picture: show it in a floating window
    Preview(Loc, String),
    /// a texture dictionary: list its textures in a floating window
    Ytd(Loc, String),
    /// a text file: edit it in a floating window
    Text(Loc, String),
    /// an archetype file: edit it in the archetype editor window
    Ytyp(Loc, String),
    /// show the Settings page (the "mods" folder notice's settings button)
    Settings,
    /// a row dragged out of the window: hand a copy to Windows' drag and drop
    DragOut(Loc, String, Kind),
}

/// what opens in a floating picture window (no mode change)
pub fn is_picture(name: &str) -> bool {
    matches!(ext(name).as_str(), "dds" | "png" | "jpg" | "jpeg" | "tga" | "bmp" | "webp")
}

pub struct Ctx<'a> {
    pub game: Option<&'a Game>,
}

/// what the viewer can open: models and texture dictionaries
pub fn is_openable(name: &str) -> bool {
    matches!(ext(name).as_str(), "ydr" | "ydd" | "yft" | "ytd")
}

/// what opens in the floating text editor
pub fn is_text(name: &str) -> bool {
    matches!(ext(name).as_str(), "meta" | "xml" | "lua" | "txt" | "json" | "md" | "cfg" | "ini" | "csv" | "log")
}

pub fn ext(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// (rank, type name) for the list's Type column and its groups: folders first, then archives, models, textures, the rest.
pub fn type_info(name: &str, kind: Kind) -> (u8, String) {
    let e = ext(name);
    let (rank, t): (u8, &str) = match kind {
        Kind::Dir => (0, "Folder"),
        Kind::Rpf => (1, "Archive (RPF)"),
        Kind::File => match e.as_str() {
            "ydr" => (2, "Drawable (YDR)"),
            "ydd" => (2, "Drawable dictionary (YDD)"),
            "yft" => (2, "Fragment (YFT)"),
            "yld" => (2, "Cloth (YLD)"),
            "ytd" => (3, "Texture dictionary (YTD)"),
            "ybn" => (4, "Collision (YBN)"),
            "ymap" => (4, "Map (YMAP)"),
            "ytyp" => (4, "Map types (YTYP)"),
            "ycd" => (4, "Clip dictionary (YCD)"),
            "ynv" => (4, "Navmesh (YNV)"),
            "ynd" => (4, "Path nodes (YND)"),
            "ymt" => (4, "Metadata (YMT)"),
            "ypt" => (4, "Particles (YPT)"),
            "awc" => (4, "Audio (AWC)"),
            "gfx" => (4, "Scaleform (GFX)"),
            "meta" => (5, "Meta file"),
            "xml" => (5, "XML file"),
            "lua" => (5, "Lua script"),
            "txt" => (5, "Text file"),
            "dat" => (5, "Data file"),
            "dds" | "png" | "jpg" | "jpeg" | "tga" | "bmp" | "webp" => (6, "Image"),
            "exe" => (7, "Application"),
            "dll" => (7, "Dynamic-link library"),
            _ => (8, ""),
        },
    };
    if t.is_empty() {
        (rank, if e.is_empty() { "File".to_string() } else { format!("{} file", e.to_uppercase()) })
    } else {
        (rank, t.to_string())
    }
}

impl Node {
    /// The installed game's folder: what classic modding tools calls GTA V.
    pub fn game_dir(game: &Game) -> Node {
        Node { name: "GTA V".into(), kind: Kind::Dir, loc: Loc::Disk(game.dir.clone()), size: 0, children: None, error: None, removable: false }
    }

    pub fn folder(path: &Path) -> Node {
        Node { name: path.display().to_string(), kind: Kind::Dir, loc: Loc::Disk(path.to_path_buf()), size: 0, children: None, error: None, removable: true }
    }

    pub fn is_container(&self) -> bool {
        matches!(self.kind, Kind::Dir | Kind::Rpf)
    }

    /// the name for the tree and the breadcrumb: an added folder shows its last part
    pub fn shown_name(&self) -> String {
        if self.removable {
            Path::new(&self.name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.name.clone())
        } else {
            self.name.clone()
        }
    }

    /// where the node lives, for tooltips and Copy path
    pub fn where_is(&self) -> String {
        match &self.loc {
            Loc::Disk(p) => p.display().to_string(),
            Loc::Arch { path, origin, .. } => format!("{origin}/{path}"),
        }
    }

    pub fn children(&self) -> &[Node] {
        self.children.as_deref().unwrap_or(&[])
    }

    pub fn children_mut(&mut self) -> &mut [Node] {
        self.children.as_deref_mut().unwrap_or(&mut [])
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// forget what was listed: the next look reads it again
    pub fn reload(&mut self) {
        self.children = None;
        self.error = None;
    }

    pub fn load_if_needed(&mut self, ctx: &Ctx) {
        if self.children.is_some() || !self.is_container() {
            return;
        }
        let res = match (&self.kind, &self.loc) {
            (Kind::Dir, Loc::Disk(p)) => list_disk(p),
            (Kind::Rpf, Loc::Disk(p)) => open_disk_archive(p, ctx),
            (Kind::Dir, Loc::Arch { ar, path, origin }) => list_arch(ar, path, origin),
            (Kind::Rpf, Loc::Arch { ar, path, origin }) => ar.child(path).map(Rc::new).and_then(|c| list_arch(&c, "", &format!("{origin}/{path}"))),
            _ => Ok(vec![]),
        };
        match res {
            Ok(c) => self.children = Some(c),
            Err(e) => {
                self.children = Some(vec![]);
                self.error = Some(format!("{e:#}"));
            }
        }
    }
}

/// The node at `path` (names from a root downwards); the folders and archives on the way are listed, the last one is not.
pub fn find_mut<'a>(roots: &'a mut [Node], path: &[String], ctx: &Ctx) -> Option<&'a mut Node> {
    let (first, rest) = path.split_first()?;
    let mut node = roots.iter_mut().find(|r| r.name == *first)?;
    for name in rest {
        node.load_if_needed(ctx);
        let kids = node.children.as_mut()?;
        node = kids.iter_mut().find(|c| c.name == *name)?;
    }
    Some(node)
}

fn node_file(name: String, size: u64, loc: Loc) -> Node {
    let kind = if name.to_lowercase().ends_with(".rpf") { Kind::Rpf } else { Kind::File };
    Node { name, kind, loc, size, children: None, error: None, removable: false }
}

fn sort(v: &mut [Node]) {
    v.sort_by(|a, b| (a.kind != Kind::Dir).cmp(&(b.kind != Kind::Dir)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

fn list_disk(dir: &Path) -> Result<Vec<Node>> {
    let mut out = vec![];
    for e in fs::read_dir(dir).with_context(|| dir.display().to_string())?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            out.push(Node { name, kind: Kind::Dir, loc: Loc::Disk(p), size: 0, children: None, error: None, removable: false });
        } else {
            out.push(node_file(name, e.metadata().map(|m| m.len()).unwrap_or(0), Loc::Disk(p)));
        }
    }
    sort(&mut out);
    Ok(out)
}

fn open_disk_archive(p: &Path, ctx: &Ctx) -> Result<Vec<Node>> {
    let ar = Rc::new(Archive::open(p, ctx.game.map(|g| g.keys()))?);
    list_arch(&ar, "", &p.file_name().unwrap_or_default().to_string_lossy())
}

fn list_arch(ar: &Rc<Archive>, prefix: &str, origin: &str) -> Result<Vec<Node>> {
    let mut dirs: Vec<String> = vec![];
    let mut files: Vec<Node> = vec![];
    for f in ar.files() {
        let Some(rest) = f.path.strip_prefix(prefix) else { continue };
        match rest.split_once('/') {
            Some((d, _)) => {
                if !dirs.iter().any(|x| x == d) {
                    dirs.push(d.to_string());
                }
            }
            None => files.push(node_file(rest.to_string(), f.size as u64, Loc::Arch { ar: ar.clone(), path: f.path.clone(), origin: origin.to_string() })),
        }
    }
    let mut out: Vec<Node> = dirs.into_iter().map(|d| Node { loc: Loc::Arch { ar: ar.clone(), path: format!("{prefix}{d}/"), origin: origin.to_string() }, name: d, kind: Kind::Dir, size: 0, children: None, error: None, removable: false }).collect();
    out.extend(files);
    sort(&mut out);
    Ok(out)
}

pub fn human(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

// ---- turning nodes into files ----------------------------------------------------------------------------------------

pub fn read(loc: &Loc) -> Result<Vec<u8>> {
    match loc {
        Loc::Disk(p) => Ok(fs::read(p)?),
        Loc::Arch { ar, path, .. } => ar.read(path),
    }
}

/// The file on disk for a node: the real file for folders on disk, a cached copy for files inside archives.
pub fn materialize(loc: &Loc, name: &str) -> Result<PathBuf> {
    match loc {
        Loc::Disk(p) => Ok(p.clone()),
        Loc::Arch { path, origin, .. } => {
            let mut h = DefaultHasher::new();
            (origin, path).hash(&mut h);
            let dir = cache_dir().join("browse").join(format!("{:016x}", h.finish()));
            fs::create_dir_all(&dir)?;
            // only the last part of the name: an archive's names may hold `\` or `..`
            let leaf = name.rsplit(['/', '\\']).next().unwrap_or(name);
            let out = rage_toolkit_core::rpf::safe_join(&dir, leaf)?;
            fs::write(&out, read(loc)?)?;
            Ok(out)
        }
    }
}

/// `<stem>.ytd` and `<stem>+hi.ytd` next to a model, as cached/real files
pub fn sibling_textures(loc: &Loc, name: &str) -> Vec<PathBuf> {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.strip_suffix("_hi").unwrap_or(&stem).to_string();
    let mut out = vec![];
    for cand in [format!("{stem}.ytd"), format!("{stem}+hi.ytd")] {
        match loc {
            Loc::Disk(p) => {
                let q = p.with_file_name(&cand);
                if q.exists() {
                    out.push(q);
                }
            }
            Loc::Arch { ar, path, origin } => {
                let dir = path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
                let full = format!("{dir}{cand}");
                if ar.contains(&full) {
                    if let Ok(p) = materialize(&Loc::Arch { ar: ar.clone(), path: full, origin: origin.clone() }, &cand) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// extract a node (file, or everything below an archive/folder) to `dest`; `game` supplies the keys for the game's own archives
pub fn extract(loc: &Loc, name: &str, kind: Kind, dest: &Path, game: Option<&Game>) -> Result<usize> {
    match (kind, loc) {
        (Kind::File, _) => {
            fs::write(dest, read(loc)?)?;
            Ok(1)
        }
        (Kind::Dir, Loc::Disk(p)) if p.is_dir() => {
            fn copy(from: &Path, to: &Path, depth: usize) -> Result<usize> {
                fs::create_dir_all(to)?;
                let mut n = 0;
                for e in fs::read_dir(from)?.flatten() {
                    let (a, b) = (e.path(), to.join(e.file_name()));
                    if a.is_dir() {
                        // a junction pointing back up the tree must not recurse until the stack overflows
                        if depth < rage_toolkit_core::search::MAX_FOLDER_DEPTH {
                            n += copy(&a, &b, depth + 1)?;
                        }
                    } else {
                        n += fs::copy(&a, &b).map(|_| 1)?;
                    }
                }
                Ok(n)
            }
            copy(p, &dest.join(name), 0)
        }
        (_, Loc::Disk(p)) => {
            // an archive on disk: the game's keys when it is one of the game's (retail archives are encrypted)
            let ar = Archive::open(p, game.map(|g| g.keys())).or_else(|_| Archive::open(p, None)).or_else(|_| bail_keys())?;
            dump(&ar, "", &dest.join(name))
        }
        (_, Loc::Arch { ar, path, .. }) => {
            if kind == Kind::Rpf {
                let child = ar.child(path)?;
                dump(&child, "", &dest.join(name))
            } else {
                dump(ar, path, &dest.join(name))
            }
        }
    }
}

fn bail_keys() -> Result<Archive> {
    bail!("this archive needs the game's keys: GTA V has to be found on this PC")
}

fn dump(ar: &Archive, prefix: &str, out: &Path) -> Result<usize> {
    dump_at(ar, prefix, out, 0)
}

fn dump_at(ar: &Archive, prefix: &str, out: &Path, depth: usize) -> Result<usize> {
    let mut n = 0;
    for f in ar.files() {
        let Some(rest) = f.path.strip_prefix(prefix) else { continue };
        // names come from the archive: one with `..` or a drive in it must not write outside `out`
        let dest = rage_toolkit_core::rpf::safe_join(out, rest)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if f.path.to_lowercase().ends_with(".rpf") && depth < rage_toolkit_core::rpf::MAX_NESTING {
            // nested archives are written as folders so the tree is browsable on disk
            if let Ok(c) = ar.child(&f.path) {
                n += dump_at(&c, "", &dest, depth + 1)?;
                continue;
            }
        }
        if let Ok(bytes) = ar.read(&f.path) {
            fs::write(&dest, bytes)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_group_folders_archives_models_textures_then_the_rest() {
        assert_eq!(type_info("x", Kind::Dir), (0, "Folder".into()));
        assert_eq!(type_info("x64e.rpf", Kind::Rpf), (1, "Archive (RPF)".into()));
        assert_eq!(type_info("adder.YFT", Kind::File), (2, "Fragment (YFT)".into()));
        assert_eq!(type_info("adder.ytd", Kind::File), (3, "Texture dictionary (YTD)".into()));
        assert_eq!(type_info("GTA5.exe", Kind::File), (7, "Application".into()));
        assert_eq!(type_info("weird.abc", Kind::File), (8, "ABC file".into()));
        assert_eq!(type_info("README", Kind::File), (8, "File".into()));
        assert!(is_openable("a.yft") && is_openable("a.YTD") && !is_openable("a.meta"));
    }

    #[test]
    fn a_path_of_names_finds_the_node() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_nodes_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/c.ytd"), b"x").unwrap();
        let mut roots = vec![Node::folder(&d)];
        let ctx = Ctx { game: None };
        let root = roots[0].name.clone();
        let p = |v: &[&str]| std::iter::once(root.clone()).chain(v.iter().map(|s| s.to_string())).collect::<Vec<_>>();
        let n = find_mut(&mut roots, &p(&["a", "b"]), &ctx).unwrap();
        assert_eq!(n.kind, Kind::Dir);
        assert!(n.children().is_empty(), "the last node is found, not listed yet");
        n.load_if_needed(&ctx);
        assert_eq!(n.children().len(), 1);
        assert_eq!(n.children()[0].name, "c.ytd");
        assert!(find_mut(&mut roots, &p(&["a", "nope"]), &ctx).is_none());
        assert!(find_mut(&mut roots, &["elsewhere".to_string()], &ctx).is_none());
        assert_eq!(roots[0].shown_name(), d.file_name().unwrap().to_string_lossy());
        fs::remove_dir_all(d).unwrap();
    }
}
