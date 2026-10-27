//! The Explorer, laid out like the classic tools' file view: a bar with back / forward / up, the path and a search box; folders (and
//! archives) in a tree on the left; the selected folder's contents in a list on the right, grouped by type, with Name / Type /
//! Size columns. Double-click a folder or archive to go in, a model or texture dictionary to look at it.
use crate::browser::{self, Action, Ctx, Kind, Loc, Node};
use eframe::egui::{self, Align2, Color32, Rect, Sense};
use rage_toolkit_core::{
    game::Game,
    mods,
    search::{self, Hit, Kind as SearchKind, Query},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
};

/// Where an edit goes: the archive file on disk, the archives inside it on the way (each path relative to the one before), and
/// the folder or file inside the last one.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub disk: PathBuf,
    pub chain: Vec<String>,
    pub inner: String,
    /// the disk archive's place in the tree (to list it again after an edit)
    node: Vec<String>,
}

/// An edit of an archive running on a thread.
struct EditJob {
    rx: mpsc::Receiver<anyhow::Result<String>>,
    node: Vec<String>,
}

/// Copy to "mods" folder, running on a thread.
struct CopyJob {
    rx: mpsc::Receiver<anyhow::Result<mods::Copied>>,
    done: Arc<AtomicU64>,
    total: u64,
    cancel: Arc<AtomicBool>,
    /// the place of the game's archive in the tree (names from the root), and the folders inside it the Explorer was in
    from: Vec<String>,
    inner: Vec<String>,
}

const ROW_H: f32 = 22.0;
const SEARCH_LIMIT: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Name,
    Type,
    Size,
}

/// One thing in the list: a folder, an archive or a file (from the current folder, or a search result).
#[derive(Clone, Debug)]
struct Item {
    name: String,
    kind: Kind,
    size: u64,
    rank: u8,
    type_name: String,
    /// search results: the folder it is in
    place: String,
    /// search results: where it is in the tree (names from a root downwards), including its own name
    path: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Row {
    Group(String),
    Item(usize),
}

/// Groups by type (folders, archives, models, textures, the rest), each sorted by the column; the Type column sorts the groups by name.
fn build_rows(items: &[Item], col: Col, desc: bool) -> Vec<Row> {
    // file types CRCK Explorer does not know (rank 8) share one group; the Type column still names each one
    fn group_of(i: &Item) -> &str {
        if i.rank == 8 { "Other files" } else { i.type_name.as_str() }
    }
    // each item lands in its group once, and its name is lowercased once (not in every comparison)
    let mut by_group: std::collections::BTreeMap<(u8, &str), Vec<usize>> = Default::default();
    for (n, i) in items.iter().enumerate() {
        by_group.entry((i.rank, group_of(i))).or_default().push(n);
    }
    let lower: Vec<String> = items.iter().map(|i| i.name.to_lowercase()).collect();
    let mut groups: Vec<((u8, &str), Vec<usize>)> = by_group.into_iter().collect();
    if col == Col::Type {
        groups.sort_by_key(|((_, t), _)| t.to_lowercase());
        if desc {
            groups.reverse();
        }
    }
    let mut rows = vec![];
    for ((_, t), mut idx) in groups {
        idx.sort_by(|&a, &b| {
            let o = match col {
                Col::Size => items[a].size.cmp(&items[b].size),
                _ => std::cmp::Ordering::Equal,
            };
            o.then_with(|| lower[a].cmp(&lower[b]))
        });
        if desc && col != Col::Type {
            idx.reverse();
        }
        rows.push(Row::Group(format!("{t}  -  {} item{}", idx.len(), if idx.len() == 1 { "" } else { "s" })));
        rows.extend(idx.into_iter().map(Row::Item));
    }
    rows
}

struct Listing {
    items: Vec<Item>,
    rows: Vec<Row>,
    error: Option<String>,
}

/// where the window has been, for the back and forward buttons
#[derive(Default)]
struct History {
    back: Vec<Vec<String>>,
    fwd: Vec<Vec<String>>,
}

impl History {
    fn go(&mut self, from: &[String]) {
        self.back.push(from.to_vec());
        self.fwd.clear();
    }

    fn back(&mut self, now: &[String]) -> Option<Vec<String>> {
        let p = self.back.pop()?;
        self.fwd.push(now.to_vec());
        Some(p)
    }

    fn forward(&mut self, now: &[String]) -> Option<Vec<String>> {
        let p = self.fwd.pop()?;
        self.back.push(now.to_vec());
        Some(p)
    }
}

pub struct Explorer {
    pub roots: Vec<Node>,
    current: Vec<String>,
    history: History,
    selected: Option<String>,
    sort: Col,
    desc: bool,
    listing: Option<Listing>,
    /// open the tree down to `current` (a few frames: lazily listed folders need one each)
    follow: u8,
    scroll_to: Option<String>,
    pub search: String,
    pub search_kind: SearchKind,
    /// the list shows these instead of the folder
    results: Option<Vec<Hit>>,
    job: Option<mpsc::Receiver<Vec<Hit>>>,
    focus_search: bool,
    /// `--show-first` (automated screenshots): go to the first result when the search is done
    pub shot_first: bool,
    /// a line for the status bar (what the last search found, ...)
    pub message: String,
    /// the settings file the added folders are remembered in (None: not remembered, as in tests and screenshots)
    persist: Option<PathBuf>,
    /// Settings: show the notice inside the game's own archives, and the "mods" folder copies go to (`set_mods`)
    warn_game_files: bool,
    mods_dir: Option<PathBuf>,
    /// the game's own archive on disk the current folder is in (not a mods copy), and its index in `current`
    game_archive: Option<(PathBuf, usize)>,
    copy: Option<CopyJob>,
    /// The classic tools' Edit mode: archives that are not the game's own can be changed
    pub edit_mode: bool,
    /// the current folder, when it is inside an archive that may be edited
    editable: Option<Place>,
    edit_job: Option<EditJob>,
    /// waiting for a new name / a yes: the item's place
    renaming: Option<(Place, String)>,
    deleting: Option<Place>,
    /// files dropped on a folder on disk that already has files of those names: the folder, and the files asking to replace them
    replacing: Option<(PathBuf, Vec<PathBuf>)>,
    /// a file or folder on disk asking to go to the Recycle Bin
    trashing: Option<PathBuf>,
    /// what was just opened from an editable archive in Edit mode: the window saves back into it
    pub opened_place: Option<Place>,
}

impl Explorer {
    pub fn new(game: Option<&Game>) -> Self {
        let roots: Vec<Node> = game.map(Node::game_dir).into_iter().collect();
        let current = roots.first().map(|r| vec![r.name.clone()]).unwrap_or_default();
        Self {
            roots,
            current,
            history: History::default(),
            selected: None,
            sort: Col::Name,
            desc: false,
            listing: None,
            follow: 3,
            scroll_to: None,
            search: String::new(),
            search_kind: SearchKind::Both,
            results: None,
            job: None,
            focus_search: false,
            shot_first: false,
            message: String::new(),
            persist: None,
            warn_game_files: true,
            mods_dir: None,
            game_archive: None,
            copy: None,
            edit_mode: false,
            editable: None,
            edit_job: None,
            renaming: None,
            deleting: None,
            replacing: None,
            trashing: None,
            opened_place: None,
        }
    }

    /// Where `path` (names from a root) is, for an edit: None unless it is inside an archive on disk that is not one of the
    /// game's own files. `item`: the last name is a file or folder to change (not a container to go into).
    fn edit_place(&mut self, path: &[String], item: bool, cx: &Ctx) -> Option<Place> {
        let k = path.iter().position(|n| n.to_lowercase().ends_with(".rpf"))?;
        if item && k + 1 >= path.len() {
            return None;
        }
        let node = browser::find_mut(&mut self.roots, &path[..=k], cx)?;
        let (Kind::Rpf, Loc::Disk(disk)) = (node.kind, &node.loc) else { return None };
        let disk = disk.clone();
        if let (Some(g), Some(m)) = (self.game_dir(), self.mods_dir.clone()) {
            if mods::game_relative(&g, &m, &disk).is_some() {
                return None;
            }
        }
        let rest = &path[k + 1..];
        let (inside, last) = if item { (&rest[..rest.len() - 1], Some(&rest[rest.len() - 1])) } else { (rest, None) };
        let (mut chain, mut cur) = (vec![], vec![]);
        for n in inside {
            cur.push(n.as_str());
            if n.to_lowercase().ends_with(".rpf") {
                chain.push(cur.join("/"));
                cur.clear();
            }
        }
        cur.extend(last.map(String::as_str));
        Some(Place { disk, chain, inner: cur.join("/"), node: path[..=k].to_vec() })
    }

    /// Run edits of one archive on a thread; when done the archive is listed again.
    fn start_edit(&mut self, place: &Place, edits: Vec<rage_toolkit_core::rpf_edit::Edit>, what: String, game: Option<&Game>) {
        if self.edit_job.is_some() {
            self.message = "Another edit is still being written".into();
            return;
        }
        let keys = game.map(|g| g.keys());
        let guard = self.game_dir().zip(self.mods_dir.clone());
        let (disk, chain) = (place.disk.clone(), place.chain.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = rage_toolkit_core::rpf_edit::edit(&disk, &chain, &edits, keys.as_deref(), guard.as_ref().map(|(g, m)| (g.as_path(), m.as_path())));
            let _ = tx.send(r.map(|rep| format!("{what}: {}", rep.summary())));
        });
        self.message = "Writing into the archive...".into();
        self.edit_job = Some(EditJob { rx, node: place.node.clone() });
    }

    fn poll_edit(&mut self, ctx: &egui::Context, cx: &Ctx) {
        let Some(job) = &self.edit_job else { return };
        let res = match job.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("the edit stopped")),
        };
        let job = self.edit_job.take().unwrap();
        self.message = match res {
            Ok(m) => m,
            Err(e) => format!("Edit failed: {e:#}"),
        };
        // list the archive again (its table changed); the folders below it are listed again when looked at
        if let Some(n) = browser::find_mut(&mut self.roots, &job.node, cx) {
            n.reload();
        }
        self.listing = None;
    }

    /// Write `data` back as the file at `place` (an editor saved its copy). False while another edit is still being written:
    /// try again later.
    pub fn save_back(&mut self, place: &Place, data: Vec<u8>, game: Option<&Game>) -> bool {
        if self.edit_job.is_some() {
            return false;
        }
        let name = place.inner.rsplit('/').next().unwrap_or("").to_string();
        self.start_edit(place, vec![rage_toolkit_core::rpf_edit::Edit::Put { path: place.inner.clone(), data }], format!("Saved {name} into {}", place.disk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), game);
        true
    }

    /// The Delete key and the Delete button: ask whether to delete the selected item from the archive (Edit mode only).
    fn delete_selected(&mut self, cx: &Ctx) {
        let Some(name) = self.selected.clone() else { return };
        // a folder on disk: to the Recycle Bin (asks first)
        if let Some(dir) = self.disk_folder(cx) {
            self.trashing = Some(dir.join(name));
            return;
        }
        if !self.edit_mode || self.editable.is_none() || self.results.is_some() {
            return;
        }
        let mut path = self.current.clone();
        path.push(name);
        self.deleting = self.edit_place(&path, true, cx);
    }

    /// An edit is being written (the window waits for it in automated screenshots).
    pub fn editing(&self) -> bool {
        self.edit_job.is_some()
    }

    /// Files dropped on the window while the Explorer is open go into the folder it shows: a folder of an archive (in Edit
    /// mode), or a folder on disk (copied in; never one of the game's own folders). False when the drop is not for it.
    pub fn take_drop(&mut self, files: &[PathBuf], game: Option<&Game>) -> bool {
        // a copy CRCK Explorer just dragged out, dropped straight back: nothing to do
        if !files.is_empty() && files.iter().all(|f| f.starts_with(rage_toolkit_core::game::cache_dir())) {
            return true;
        }
        if self.results.is_some() {
            return false; // search results: not one folder
        }
        let Some(place) = self.editable.clone() else {
            if self.current.iter().any(|n| n.to_lowercase().ends_with(".rpf")) && files.iter().any(|f| f.is_file()) {
                self.message = "This is one of the game's own archives, which CRCK Explorer never writes to: copy it to the mods folder (the bar above) and drop the files into the copy".into();
                return true;
            }
            return self.drop_into_folder(files, game);
        };
        if !self.edit_mode {
            self.message = "Turn on Edit mode (top left) to put the dropped files into this archive".into();
            return true;
        }
        let files: Vec<&PathBuf> = files.iter().filter(|p| p.is_file()).collect();
        if files.is_empty() {
            return false;
        }
        self.add_files(&place, &files, game);
        true
    }

    /// The small windows that ask for a new name, or whether to delete.
    fn edit_dialogs(&mut self, ctx: &egui::Context, game: Option<&Game>) {
        if let Some((place, text)) = self.renaming.as_mut() {
            let (mut open, mut go) = (true, false);
            let old = place.inner.rsplit('/').next().unwrap_or("").to_string();
            egui::Window::new("Rename").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label(format!("New name for {old}:"));
                let r = ui.text_edit_singleline(text);
                r.request_focus();
                go = ui.button("Rename").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            });
            if go {
                let (place, to) = self.renaming.take().unwrap();
                let to = to.trim().to_string();
                if !to.is_empty() && to != old {
                    self.start_edit(&place, vec![rage_toolkit_core::rpf_edit::Edit::Rename { path: place.inner.clone(), to: to.clone() }], format!("Renamed {old} to {to}"), game);
                }
            } else if !open {
                self.renaming = None;
            }
        }
        if let Some(place) = self.deleting.clone() {
            let (mut open, mut yes, mut no) = (true, false, false);
            egui::Window::new("Delete").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label(format!("Delete {} from the archive?", place.inner));
                ui.horizontal(|ui| {
                    yes = ui.button(egui::RichText::new("Delete").color(crate::theme::err())).clicked();
                    no = ui.button("Keep it").clicked();
                });
            });
            if yes {
                self.deleting = None;
                self.start_edit(&place, vec![rage_toolkit_core::rpf_edit::Edit::Delete { path: place.inner.clone() }], format!("Deleted {}", place.inner), game);
            } else if no || !open {
                self.deleting = None;
            }
        }
        if let Some(path) = self.trashing.clone() {
            let (mut open, mut yes, mut no) = (true, false, false);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            egui::Window::new("Delete").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label(format!("Move {name} to the Recycle Bin?"));
                ui.horizontal(|ui| {
                    yes = ui.button(egui::RichText::new("Move to Recycle Bin").color(crate::theme::err())).clicked();
                    no = ui.button("Keep it").clicked();
                });
            });
            if yes {
                self.trashing = None;
                self.message = match recycle(&path) {
                    Ok(()) => format!("{name} is in the Recycle Bin"),
                    Err(e) => format!("{name} was not deleted: {e}"),
                };
                let (current, cx) = (self.current.clone(), Ctx { game });
                if let Some(n) = browser::find_mut(&mut self.roots, &current, &cx) {
                    n.reload();
                }
                self.listing = None;
                self.selected = None;
            } else if no || !open {
                self.trashing = None;
            }
        }
        if let Some((dir, files)) = self.replacing.clone() {
            let (mut open, mut yes, mut no) = (true, false, false);
            let names: Vec<String> = files.iter().filter_map(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
            egui::Window::new("Replace files?").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
                ui.label(format!("Already in {}: {}", dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), names.join(", ")));
                ui.horizontal(|ui| {
                    yes = ui.button(egui::RichText::new("Replace").color(crate::theme::err())).clicked();
                    no = ui.button("Keep the old ones").clicked();
                });
            });
            if yes {
                self.replacing = None;
                self.copy_into(&dir, &files, &Ctx { game });
            } else if no || !open {
                self.replacing = None;
            }
        }
    }

    /// The folder on disk the Explorer shows, when CRCK Explorer may change it: not an archive, not search results, not one of
    /// the game's own folders (its mods folder is fine).
    fn disk_folder(&mut self, cx: &Ctx) -> Option<PathBuf> {
        if self.results.is_some() || self.current.is_empty() {
            return None;
        }
        let current = self.current.clone();
        let node = browser::find_mut(&mut self.roots, &current, cx)?;
        let (Kind::Dir, Loc::Disk(dir)) = (node.kind, &node.loc) else { return None };
        let dir = dir.clone();
        if let Some(g) = self.game_dir() {
            let m = self.mods_dir.clone().unwrap_or_else(|| mods::default_dir(&g));
            if mods::game_relative(&g, &m, &dir.join("x")).is_some() {
                return None;
            }
        }
        Some(dir)
    }

    /// Files dropped while the Explorer shows a folder on disk: copied into it. Names already there wait for the Replace
    /// question; the game's own folders are refused.
    fn drop_into_folder(&mut self, files: &[PathBuf], game: Option<&Game>) -> bool {
        let files: Vec<&PathBuf> = files.iter().filter(|p| p.is_file()).collect();
        if files.is_empty() || self.current.is_empty() {
            return false;
        }
        let cx = Ctx { game };
        let current = self.current.clone();
        let Some(node) = browser::find_mut(&mut self.roots, &current, &cx) else { return false };
        let (Kind::Dir, Loc::Disk(dir)) = (node.kind, &node.loc) else { return false };
        let dir = dir.clone();
        // a file put here would be one of the game's own files: refused (the mods folder and the user's folders are fine)
        if let Some(g) = self.game_dir() {
            let m = self.mods_dir.clone().unwrap_or_else(|| mods::default_dir(&g));
            if mods::game_relative(&g, &m, &dir.join("x")).is_some() {
                self.message = "This is one of the game's own folders, which CRCK Explorer never writes to: drop the files into the mods folder or a folder of yours".into();
                return true;
            }
        }
        let (mut fresh, mut clash) = (vec![], vec![]);
        for f in files {
            let Some(name) = f.file_name() else { continue };
            let to = dir.join(name);
            if !to.exists() {
                fresh.push(f.clone());
            } else if std::fs::canonicalize(f).ok() != std::fs::canonicalize(&to).ok() {
                clash.push(f.clone());
            } // else: dropped on the folder it is already in
        }
        if fresh.is_empty() && clash.is_empty() {
            self.message = "Those files are in this folder already".into();
        }
        self.copy_into(&dir, &fresh, &cx);
        if !clash.is_empty() {
            self.replacing = Some((dir, clash));
        }
        true
    }

    /// Copy `files` into the folder `dir` (overwriting), then list the folder again.
    fn copy_into(&mut self, dir: &Path, files: &[PathBuf], cx: &Ctx) {
        if files.is_empty() {
            return;
        }
        let (mut done, mut errors) = (vec![], vec![]);
        for f in files {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::copy(f, dir.join(&name)) {
                Ok(_) => done.push(name),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
        self.message = if errors.is_empty() { format!("Copied {} into {folder}", done.join(", ")) } else { format!("Copied {} file(s) into {folder}; not copied: {}", done.len(), errors.join("; ")) };
        let current = self.current.clone();
        if let Some(n) = browser::find_mut(&mut self.roots, &current, cx) {
            n.reload();
        }
        self.listing = None;
    }

    fn add_files(&mut self, place: &Place, files: &[&PathBuf], game: Option<&Game>) {
        let mut edits = vec![];
        for f in files {
            match std::fs::read(f) {
                Ok(data) => {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let path = if place.inner.is_empty() { name } else { format!("{}/{name}", place.inner) };
                    edits.push(rage_toolkit_core::rpf_edit::Edit::Put { path, data });
                }
                Err(e) => {
                    self.message = format!("{}: {e}", f.display());
                    return;
                }
            }
        }
        self.start_edit(place, edits, format!("Added {} file(s)", files.len()), game);
    }

    /// The two Settings the "mods" notice follows (called every frame; a new mods folder is looked at again).
    pub fn set_mods(&mut self, warn: bool, dir: Option<PathBuf>) {
        self.warn_game_files = warn;
        if self.mods_dir != dir {
            self.mods_dir = dir;
            self.listing = None;
        }
    }

    /// the installed game's folder (the root that is not an added folder)
    fn game_dir(&self) -> Option<PathBuf> {
        self.roots.iter().find(|r| !r.removable).and_then(|r| if let Loc::Disk(p) = &r.loc { Some(p.clone()) } else { None })
    }

    /// The game's own archive on disk that the current folder is in, when it is not a copy in the mods folder.
    fn find_game_archive(&mut self, cx: &Ctx) -> Option<(PathBuf, usize)> {
        let (game_dir, mods_dir) = (self.game_dir()?, self.mods_dir.clone()?);
        let k = self.current.iter().position(|n| n.to_lowercase().ends_with(".rpf"))?;
        let node = browser::find_mut(&mut self.roots, &self.current[..=k], cx)?;
        match (&node.kind, &node.loc) {
            (Kind::Rpf, Loc::Disk(p)) if mods::game_relative(&game_dir, &mods_dir, p).is_some() => Some((p.clone(), k)),
            _ => None,
        }
    }

    /// Copy the game's archive the Explorer is in to the mods folder (on a thread), then go to the same place in the copy.
    fn start_copy(&mut self, archive: PathBuf, k: usize) {
        let (Some(game_dir), Some(mods_dir)) = (self.game_dir(), self.mods_dir.clone()) else { return };
        let total = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
        let (done, cancel) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        let (tx, rx) = mpsc::channel();
        let (d, c) = (done.clone(), cancel.clone());
        std::thread::spawn(move || {
            let r = mods::copy_to_mods(&game_dir, &mods_dir, &archive, &mut |n, _| {
                d.store(n, Ordering::Relaxed);
                !c.load(Ordering::Relaxed)
            });
            let _ = tx.send(r);
        });
        self.copy = Some(CopyJob { rx, done, total, cancel, from: self.current[..=k].to_vec(), inner: self.current[k + 1..].to_vec() });
    }

    /// The place in the tree of a file on disk: below the game's folder, or below the mods folder (added to the tree when it
    /// is somewhere else).
    fn place_of(&mut self, p: &Path) -> Option<Vec<String>> {
        if let Some(rel) = self.game_dir().and_then(|g| p.strip_prefix(&g).ok().map(Path::to_path_buf)) {
            return self.components_of(&Hit { path: rel.to_string_lossy().into_owned(), size: 0, is_dir: true });
        }
        if let Some(m) = self.mods_dir.clone().filter(|m| m.is_dir()) {
            self.add_folder(&m, false);
        }
        self.components_of(&Hit { path: p.display().to_string(), size: 0, is_dir: true })
    }

    /// List again the folders on the way to `path` that do not show its next part yet (a new copy in the mods folder).
    fn refresh_to(&mut self, path: &[String], cx: &Ctx) {
        for i in 1..path.len() {
            if browser::find_mut(&mut self.roots, &path[..=i], cx).is_none() {
                if let Some(n) = browser::find_mut(&mut self.roots, &path[..i], cx) {
                    n.reload();
                }
            }
        }
    }

    fn poll_copy(&mut self, ctx: &egui::Context, cx: &Ctx) {
        let Some(job) = &self.copy else { return };
        let res = match job.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("the copy stopped")),
        };
        let job = self.copy.take().unwrap();
        match res {
            Ok(c) => {
                let dest = c.path().to_path_buf();
                self.message = match c {
                    mods::Copied::New(_) => format!("Copied to {}", dest.display()),
                    mods::Copied::Existing(_) => format!("{} was there already and was left as it is", dest.display()),
                };
                // still where the copy was asked for: go to the same place in the copy
                if self.current.starts_with(&job.from) {
                    if let Some(mut path) = self.place_of(&dest) {
                        self.refresh_to(&path, cx);
                        path.extend(job.inner);
                        self.goto(path);
                    }
                }
            }
            Err(e) => self.message = format!("Copy to \"mods\" folder: {e:#}"),
        }
    }

    /// A one-line bar inside the game's own archives: it is read-only here; copy it to the mods folder to change it.
    fn mods_notice(&mut self, ui: &mut egui::Ui, archive: &Path, k: usize, actions: &mut Vec<Action>) {
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut start = false;
        let button = |text: &str| egui::Button::new(text);
        // amber: seen at a glance, without the alarm of a red box
        let warn = crate::theme::warn();
        egui::Frame::new().fill(warn.gamma_multiply(0.14)).stroke(egui::Stroke::new(1.5, warn.gamma_multiply(0.8))).corner_radius(6.0).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("READ-ONLY").strong().size(14.0).color(warn));
                ui.label(egui::RichText::new(format!("{name} is a game file. To change what the game loads, edit a copy in mods.")).color(crate::theme::text()))
                    .on_hover_text("CRCK Explorer never writes into the game's own archives; edits here go to copies in its cache.");
                if let Some(job) = &self.copy {
                    let done = job.done.load(Ordering::Relaxed);
                    let frac = if job.total == 0 { 0.0 } else { done as f32 / job.total as f32 };
                    ui.add(egui::ProgressBar::new(frac).desired_width(260.0).text(format!("Copying: {} of {}", browser::human(done), browser::human(job.total))));
                    if ui.add(button("Cancel")).clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                } else {
                    let dest = self.game_dir().zip(self.mods_dir.clone()).and_then(|(g, m)| mods::target(&g, &m, archive));
                    let size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
                    let tip = match &dest {
                        Some(d) if d.exists() => format!("{} is there already: go to it (it is not overwritten)", d.display()),
                        Some(d) => format!("Copies {name} ({}) to {} and opens the copy here", browser::human(size), d.display()),
                        None => "No mods folder: choose one in Settings".to_string(),
                    };
                    if ui.add_enabled(dest.is_some(), button("Copy to mods")).on_hover_text(tip).clicked() {
                        start = true;
                    }
                }
                if ui.add(button("Settings")).on_hover_text("The mods folder, and switching this bar off").clicked() {
                    actions.push(Action::Settings);
                }
            });
        });
        if start {
            self.start_copy(archive.to_path_buf(), k);
        }
        ui.add_space(6.0);
    }

    /// Bring back the folders added in earlier runs (the ones that still exist), and keep remembering from now on.
    pub fn restore(&mut self, settings: &Path) {
        for d in crate::settings::folders(settings).iter().filter(|d| d.is_dir()) {
            self.add_folder(d, false);
        }
        self.persist = Some(settings.to_path_buf());
    }

    fn save_folders(&self) {
        if let Some(p) = &self.persist {
            let dirs: Vec<PathBuf> = self.roots.iter().filter(|r| r.removable).filter_map(|r| if let Loc::Disk(p) = &r.loc { Some(p.clone()) } else { None }).collect();
            crate::settings::save_folders(p, &dirs);
        }
    }

    /// `--search` (automated screenshots), or a search started from code
    pub fn search_for(&mut self, text: &str, kind: SearchKind, show_first: bool, game: Option<&Game>) {
        self.search = text.to_string();
        self.search_kind = kind;
        self.shot_first = show_first;
        self.start_search(game);
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    // ---- places -----------------------------------------------------------------------------------------------

    pub fn goto(&mut self, path: Vec<String>) {
        if path != self.current {
            let from = std::mem::replace(&mut self.current, path);
            self.history.go(&from);
        }
        self.results = None;
        self.listing = None;
        self.selected = None;
        self.follow = 3;
    }

    /// is there somewhere to go forward to (the Viewer's way back to the Explorer is separate)
    pub fn can_forward(&self) -> bool {
        !self.history.fwd.is_empty()
    }

    fn jump(&mut self, path: Vec<String>) {
        self.current = path;
        self.results = None;
        self.listing = None;
        self.selected = None;
        self.follow = 3;
    }

    fn up(&mut self) {
        if !self.current.is_empty() {
            let mut p = self.current.clone();
            let name = p.pop();
            self.goto(p);
            self.selected = name.clone();
            self.scroll_to = name;
        }
    }

    /// `--add-folder`, the Add folder button and dropping a folder on the window
    pub fn add_folder(&mut self, dir: &Path, go: bool) {
        let n = Node::folder(dir);
        if !self.roots.iter().any(|r| r.name == n.name) {
            let name = n.name.clone();
            self.roots.push(n);
            self.save_folders();
            if go {
                self.goto(vec![name]);
            }
        } else if go {
            self.goto(vec![n.name]);
        }
    }

    /// the path of names for something found by a search
    fn components_of(&self, hit: &Hit) -> Option<Vec<String>> {
        let norm = hit.path.replace('\\', "/");
        if Path::new(&hit.path).is_absolute() {
            for r in self.roots.iter().filter(|r| r.removable) {
                let Loc::Disk(p) = &r.loc else { continue };
                let base = p.display().to_string().replace('\\', "/");
                if let Some(rest) = norm.strip_prefix(base.trim_end_matches('/')).filter(|rest| rest.starts_with('/')) {
                    return Some(std::iter::once(r.name.clone()).chain(rest.split('/').filter(|s| !s.is_empty()).map(str::to_string)).collect());
                }
            }
            None
        } else {
            let game = self.roots.iter().find(|r| !r.removable)?;
            Some(std::iter::once(game.name.clone()).chain(norm.split('/').filter(|s| !s.is_empty()).map(str::to_string)).collect())
        }
    }

    fn item_path(&self, i: &Item) -> Vec<String> {
        if self.results.is_some() {
            i.path.clone()
        } else {
            let mut p = self.current.clone();
            p.push(i.name.clone());
            p
        }
    }

    // ---- search -----------------------------------------------------------------------------------------------

    fn start_search(&mut self, game: Option<&Game>) {
        let Some(q) = Query::parse(&self.search, self.search_kind) else {
            self.results = None;
            self.listing = None;
            return;
        };
        let (game, disk) = (game.cloned(), self.roots.iter().filter(|r| r.removable).filter_map(|r| if let Loc::Disk(p) = &r.loc { Some(p.clone()) } else { None }).collect::<Vec<_>>());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut hits = vec![];
            for d in &disk {
                search::search_disk(d, &q, SEARCH_LIMIT, &mut hits);
            }
            if let Some(g) = game {
                hits.extend(g.search(&q, SEARCH_LIMIT));
            }
            let _ = tx.send(hits);
        });
        self.job = Some(rx);
        self.message = format!("Searching for '{}'...", self.search.trim());
    }

    fn poll(&mut self, ctx: &egui::Context) {
        use mpsc::TryRecvError;
        let Some(rx) = &self.job else { return };
        match rx.try_recv() {
            Ok(hits) => {
                self.message = format!("{} match(es) for '{}'{}", hits.len(), self.search.trim(), if hits.len() >= SEARCH_LIMIT { " (a lot: only the first ones are kept, narrow the search)" } else { "" });
                self.job = None;
                self.results = Some(hits);
                self.listing = None;
                self.selected = None;
                if std::mem::take(&mut self.shot_first) {
                    if let Some(h) = self.results.as_ref().and_then(|r| r.first()).cloned() {
                        self.show_hit(&h);
                    }
                }
            }
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(TryRecvError::Disconnected) => self.job = None,
        }
    }

    /// Go to the folder a search result is in and select it there.
    fn show_hit(&mut self, h: &Hit) {
        let Some(mut comps) = self.components_of(h) else {
            self.message = format!("{} is not under anything in the tree", h.path);
            return;
        };
        let name = comps.last().cloned();
        if h.is_dir {
            // a folder or archive: go into it
            self.goto(comps);
        } else {
            comps.pop();
            self.goto(comps);
            self.selected = name.clone();
            self.scroll_to = name;
        }
    }

    // ---- the list ---------------------------------------------------------------------------------------------

    fn rebuild(&mut self, ctx: &Ctx) {
        let mut error = None;
        let items: Vec<Item> = if let Some(hits) = &self.results {
            hits.iter()
                .filter_map(|h| {
                    let path = self.components_of(h)?;
                    let name = path.last()?.clone();
                    let kind = if h.is_dir { if name.to_lowercase().ends_with(".rpf") { Kind::Rpf } else { Kind::Dir } } else { Kind::File };
                    let (rank, type_name) = browser::type_info(&name, kind);
                    let place = path[..path.len() - 1].iter().skip(1).cloned().collect::<Vec<_>>().join("/");
                    Some(Item { name, kind, size: h.size, rank, type_name, place, path })
                })
                .collect()
        } else if self.current.is_empty() {
            self.roots.iter().map(|n| Item { name: n.name.clone(), kind: Kind::Dir, size: 0, rank: 0, type_name: "Folder".into(), place: String::new(), path: vec![n.name.clone()] }).collect()
        } else {
            match browser::find_mut(&mut self.roots, &self.current, ctx) {
                Some(node) => {
                    node.load_if_needed(ctx);
                    error = node.error().map(str::to_string);
                    node.children()
                        .iter()
                        .map(|c| {
                            let (rank, type_name) = browser::type_info(&c.name, c.kind);
                            Item { name: c.name.clone(), kind: c.kind, size: c.size, rank, type_name, place: String::new(), path: vec![] }
                        })
                        .collect()
                }
                None => {
                    error = Some("This folder is not there any more.".into());
                    vec![]
                }
            }
        };
        let rows = build_rows(&items, self.sort, self.desc);
        self.listing = Some(Listing { items, rows, error });
        self.game_archive = if self.results.is_none() { self.find_game_archive(ctx) } else { None };
        let here = self.current.clone();
        self.editable = if self.results.is_none() { self.edit_place(&here, false, ctx) } else { None };
    }

    pub fn status_line(&self) -> String {
        let n = self.listing.as_ref().map_or(0, |l| l.items.len());
        let sel = match &self.selected {
            Some(s) => format!("Selected: 1 ({s})"),
            None => "Selected: 0".to_string(),
        };
        format!("{n} items shown  |  {sel}")
    }

    // ---- drawing ----------------------------------------------------------------------------------------------

    /// The whole Explorer. Returns what the window should do (open a model, extract, ...).
    pub fn ui(&mut self, ui: &mut egui::Ui, game: Option<&Game>) -> Vec<Action> {
        let ectx = ui.ctx().clone();
        let cx = Ctx { game };
        let mut actions: Vec<Action> = vec![];
        self.poll(&ectx);
        self.poll_copy(&ectx, &cx);
        self.poll_edit(&ectx, &cx);
        self.edit_dialogs(&ectx, game);
        self.keys(&ectx, &cx, &mut actions);
        egui::Panel::top("explorer_nav").frame(crate::theme::panel_frame()).show(ui, |ui| self.nav_bar(ui, game));
        egui::Panel::left("explorer_tree").default_size(300.0).frame(crate::theme::panel_frame()).show(ui, |ui| self.tree_pane(ui, &cx, &mut actions));
        egui::CentralPanel::default().show(ui, |ui| self.list_pane(ui, &cx, game, &mut actions));
        self.follow = self.follow.saturating_sub(1);
        actions
    }

    fn keys(&mut self, ctx: &egui::Context, cx: &Ctx, actions: &mut Vec<Action>) {
        // the mouse's back / forward side buttons
        let (m_back, m_fwd) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        if m_back {
            if let Some(p) = self.history.back(&self.current) {
                self.jump(p);
            }
        }
        if m_fwd {
            if let Some(p) = self.history.forward(&self.current) {
                self.jump(p);
            }
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (back, fwd, up, enter, f5, find, del) = ctx.input(|i| {
            (
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Backspace) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp)),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::F5),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::Delete),
            )
        });
        if del {
            self.delete_selected(cx);
        }
        if back {
            if let Some(p) = self.history.back(&self.current) {
                self.jump(p);
            }
        }
        if fwd {
            if let Some(p) = self.history.forward(&self.current) {
                self.jump(p);
            }
        }
        if up {
            self.up();
        }
        if f5 {
            self.reload_current(cx);
        }
        if find {
            self.focus_search = true;
        }
        if enter {
            if let Some(name) = self.selected.clone() {
                self.open_named(&name, cx, actions);
            }
        }
    }

    fn reload_current(&mut self, cx: &Ctx) {
        if let Some(n) = browser::find_mut(&mut self.roots, &self.current, cx) {
            n.reload();
        }
        self.listing = None;
    }

    fn nav_bar(&mut self, ui: &mut egui::Ui, game: Option<&Game>) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.history.back.is_empty(), egui::Button::new(" < ")).on_hover_text("Back (Alt+Left)").clicked() {
                if let Some(p) = self.history.back(&self.current) {
                    self.jump(p);
                }
            }
            if ui.add_enabled(!self.history.fwd.is_empty(), egui::Button::new(" > ")).on_hover_text("Forward (Alt+Right)").clicked() {
                if let Some(p) = self.history.forward(&self.current) {
                    self.jump(p);
                }
            }
            ui.toggle_value(&mut self.edit_mode, "Edit mode").on_hover_text("Like the classic tools' Edit mode: add, replace, rename and delete files inside archives that are not the game's own (a copy in the mods folder, your own archives). Edited archives are saved unencrypted (OPEN).");
            if ui.add_enabled(!self.current.is_empty(), egui::Button::new(" Up ")).on_hover_text("Up one folder (Backspace)").clicked() {
                self.up();
            }
            ui.separator();
            // the path: every part goes to that folder
            let mut go: Option<Vec<String>> = None;
            if ui.add(egui::Button::new(egui::RichText::new("CRCK Explorer").color(crate::theme::dim())).frame(false)).clicked() {
                go = Some(vec![]);
            }
            for i in 0..self.current.len() {
                ui.label(egui::RichText::new(">").color(crate::theme::dim()));
                let shown = if i == 0 { self.roots.iter().find(|r| r.name == self.current[0]).map(Node::shown_name).unwrap_or_else(|| self.current[0].clone()) } else { self.current[i].clone() };
                if ui.add(egui::Button::new(shown).frame(false)).clicked() {
                    go = Some(self.current[..=i].to_vec());
                }
            }
            if let Some(p) = go {
                self.goto(p);
            }
            // the search box on the right
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.job.is_some();
                if busy {
                    ui.spinner();
                }
                if self.results.is_some() && ui.small_button("x").on_hover_text("Close the search results").clicked() {
                    self.results = None;
                    self.listing = None;
                    self.message.clear();
                }
                egui::ComboBox::from_id_salt("explorer_kind").selected_text(match self.search_kind {
                    SearchKind::Both => "All",
                    SearchKind::Files => "Files",
                    SearchKind::Folders => "Folders",
                }).width(70.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.search_kind, SearchKind::Both, "All").on_hover_text("File names and folder names");
                    ui.selectable_value(&mut self.search_kind, SearchKind::Files, "Files");
                    ui.selectable_value(&mut self.search_kind, SearchKind::Folders, "Folders").on_hover_text("Archives count as folders");
                });
                let r = ui
                    .add(egui::TextEdit::singleline(&mut self.search).hint_text("Search (Ctrl+F)").desired_width(220.0))
                    .on_hover_text("All words must match. * and ? make a pattern for the whole name (*.ytd). A word with / is looked for in the whole path (dlcpacks/mpheist).");
                if std::mem::take(&mut self.focus_search) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.start_search(game);
                }
            });
        });
        ui.add_space(2.0);
    }

    fn tree_pane(&mut self, ui: &mut egui::Ui, cx: &Ctx, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            if ui.button("Add folder...").on_hover_text("Browse a folder on your disk, for example a mod project").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.add_folder(&p, true);
                }
            }
            if ui.button("Refresh").on_hover_text("List this folder again (F5)").clicked() {
                self.reload_current(cx);
            }
        });
        ui.separator();
        let mut t = TreeCx { current: &self.current, follow: self.follow > 0, go: None, remove: None, actions: vec![] };
        egui::ScrollArea::both().id_salt("explorer_tree_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut path = vec![];
            for root in self.roots.iter_mut() {
                tree_node(ui, root, &mut path, &mut t, cx);
            }
            if self.roots.is_empty() {
                ui.weak("GTA V was not found on this PC. Use Add folder... to browse a folder.");
            }
        });
        let (go, remove, mut acts) = (t.go, t.remove, t.actions);
        actions.append(&mut acts);
        if let Some(name) = remove {
            self.roots.retain(|r| !(r.removable && r.name == name));
            self.save_folders();
            if self.current.first() == Some(&name) {
                self.goto(vec![]);
            }
        }
        if let Some(p) = go {
            self.goto(p);
        }
    }

    fn list_pane(&mut self, ui: &mut egui::Ui, cx: &Ctx, game: Option<&Game>, actions: &mut Vec<Action>) {
        if self.listing.is_none() {
            self.rebuild(cx);
        }
        let results_mode = self.results.is_some();
        if let Some((archive, k)) = self.game_archive.clone().filter(|_| self.warn_game_files && !results_mode) {
            self.mods_notice(ui, &archive, k, actions);
        }
        let edit_here = self.edit_mode && self.editable.is_some() && !results_mode;
        let disk_here = self.disk_folder(cx).is_some();
        if disk_here {
            ui.horizontal(|ui| {
                if ui.add_enabled(self.selected.is_some(), egui::Button::new("Delete")).on_hover_text("Move the selected file or folder to the Recycle Bin (asks first; also the Delete key)").clicked() {
                    self.delete_selected(cx);
                }
            });
        }
        if let Some(place) = self.editable.clone().filter(|_| edit_here) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Edit mode").strong().color(crate::theme::warn()));
                ui.label(format!("changes are written into {}", place.disk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                if self.edit_job.is_some() {
                    ui.spinner();
                } else if ui.add_enabled(self.selected.is_some(), egui::Button::new("Delete")).on_hover_text("Delete the selected file or folder from the archive (asks first; also the Delete key)").clicked() {
                    self.delete_selected(cx);
                } else if ui.button("Add files...").on_hover_text("Put files into this folder (a file of the same name is replaced). You can also drop files on the window.").clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        let files: Vec<&PathBuf> = files.iter().collect();
                        self.add_files(&place, &files, game);
                    }
                }
            });
        }
        if results_mode {
            ui.horizontal(|ui| {
                ui.strong(format!("Search results for '{}'", self.search.trim()));
                ui.weak("Double-click opens or goes in; right-click > Show in folder");
                if ui.small_button("Close").clicked() {
                    self.results = None;
                    self.listing = None;
                }
            });
        }
        let (sort, desc) = (self.sort, self.desc);
        let avail = ui.available_width();
        let (type_w, size_w) = (200.0f32, 90.0f32);
        let place_w = if results_mode { (avail * 0.34).clamp(180.0, 460.0) } else { 0.0 };
        let name_w = (avail - type_w - size_w - place_w - 12.0).max(120.0);

        // column titles: click to sort
        let (hrect, _) = ui.allocate_exact_size(egui::vec2(avail, 24.0), Sense::hover());
        let cols = [(Col::Name, "Name", hrect.left() + 8.0, name_w), (Col::Type, "Type", hrect.left() + 8.0 + name_w, type_w), (Col::Size, "Size", hrect.left() + 8.0 + name_w + type_w, size_w)];
        let mut clicked_col = None;
        for (c, title, x, w) in cols {
            let r = Rect::from_min_size(egui::pos2(x - 4.0, hrect.top()), egui::vec2(w, 24.0));
            let resp = ui.interact(r, ui.scope_id().with(("explorer_col", title)), Sense::click());
            let arrow = if sort == c { if desc { "  v" } else { "  ^" } } else { "" };
            let colour = if resp.hovered() { crate::theme::text() } else { crate::theme::text() };
            let align = if c == Col::Size { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            let pos = if c == Col::Size { egui::pos2(x + w - 14.0, hrect.center().y) } else { egui::pos2(x, hrect.center().y) };
            ui.painter().text(pos, align, format!("{title}{arrow}"), egui::FontId::proportional(13.0), colour);
            if resp.clicked() {
                clicked_col = Some(c);
            }
        }
        if results_mode {
            ui.painter().text(egui::pos2(hrect.left() + 8.0 + name_w + type_w + size_w + 6.0, hrect.center().y), Align2::LEFT_CENTER, "Location", egui::FontId::proportional(13.0), crate::theme::text());
        }
        ui.painter().line_segment([egui::pos2(hrect.left(), hrect.bottom()), egui::pos2(hrect.right(), hrect.bottom())], egui::Stroke::new(1.0, crate::theme::border()));

        let Some(listing) = self.listing.as_ref() else { return };
        if let Some(e) = &listing.error {
            ui.colored_label(crate::theme::err(), e);
        }
        if listing.items.is_empty() && listing.error.is_none() {
            ui.add_space(8.0);
            ui.weak(if results_mode { "Nothing matched." } else { "This folder is empty." });
        }
        let (mut click, mut dbl, mut drag): (Option<usize>, Option<usize>, Option<usize>) = (None, None, None);
        let mut menu: Option<(usize, Menu)> = None;
        let selected = self.selected.clone();
        let mut area = egui::ScrollArea::vertical().id_salt("explorer_list").auto_shrink([false, false]);
        if let Some(name) = &self.scroll_to {
            if let Some(pos) = listing.rows.iter().position(|r| matches!(r, Row::Item(i) if &listing.items[*i].name == name)) {
                area = area.vertical_scroll_offset((pos as f32 * ROW_H - 80.0).max(0.0));
            }
        }
        self.scroll_to = None;
        area.show_rows(ui, ROW_H, listing.rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for ri in range {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                match &listing.rows[ri] {
                    Row::Group(text) => {
                        ui.painter().text(egui::pos2(rect.left() + 8.0, rect.center().y + 3.0), Align2::LEFT_CENTER, text, egui::FontId::proportional(14.0), crate::theme::info());
                        ui.painter().line_segment([egui::pos2(rect.left() + 8.0, rect.bottom() - 1.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 1.0)], egui::Stroke::new(1.0, crate::theme::border()));
                    }
                    Row::Item(i) => {
                        let it = &listing.items[*i];
                        let is_sel = selected.as_deref() == Some(it.name.as_str());
                        if is_sel {
                            ui.painter().rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, crate::theme::surface_hi());
                        }
                        paint_icon(ui.painter(), Rect::from_min_size(egui::pos2(rect.left() + 8.0, rect.center().y - 7.0), egui::vec2(16.0, 14.0)), it.kind, it.rank);
                        let text_col = if is_sel { crate::theme::text() } else { crate::theme::text() };
                        let dim = if is_sel { crate::theme::text() } else { crate::theme::dim() };
                        let font = egui::FontId::proportional(13.5);
                        let clip = |x: f32, w: f32| ui.painter().with_clip_rect(Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, ROW_H)));
                        let x0 = rect.left() + 8.0;
                        clip(x0 + 22.0, name_w - 26.0).text(egui::pos2(x0 + 24.0, rect.center().y), Align2::LEFT_CENTER, &it.name, font.clone(), text_col);
                        clip(x0 + name_w, type_w - 6.0).text(egui::pos2(x0 + name_w, rect.center().y), Align2::LEFT_CENTER, &it.type_name, font.clone(), dim);
                        if it.kind != Kind::Dir {
                            ui.painter().text(egui::pos2(x0 + name_w + type_w + size_w - 14.0, rect.center().y), Align2::RIGHT_CENTER, browser::human(it.size), font.clone(), dim);
                        }
                        if results_mode {
                            clip(x0 + name_w + type_w + size_w + 6.0, place_w - 10.0).text(egui::pos2(x0 + name_w + type_w + size_w + 6.0, rect.center().y), Align2::LEFT_CENTER, &it.place, font, dim);
                        }
                        if resp.clicked() {
                            click = Some(*i);
                        }
                        if resp.double_clicked() {
                            dbl = Some(*i);
                        }
                        if resp.drag_started() {
                            drag = Some(*i);
                        }
                        let resp = resp.on_hover_text(if results_mode { format!("{}/{}", it.place, it.name) } else { it.name.clone() });
                        resp.context_menu(|ui| {
                            let open_label = if it.kind == Kind::File { "Open" } else { "Open folder" };
                            if (it.kind != Kind::File || browser::is_openable(&it.name) || browser::is_picture(&it.name) || browser::is_text(&it.name) || browser::ext(&it.name) == "ytyp") && ui.button(open_label).clicked() {
                                menu = Some((*i, Menu::Open));
                                ui.close();
                            }
                            if results_mode && ui.button("Show in folder").clicked() {
                                menu = Some((*i, Menu::ShowInFolder));
                                ui.close();
                            }
                            if browser::ext(&it.name) == "ytd" && ui.button("Add to current textures").clicked() {
                                menu = Some((*i, Menu::AddTextures));
                                ui.close();
                            }
                            if ui.button("Extract...").clicked() {
                                menu = Some((*i, Menu::Extract));
                                ui.close();
                            }
                            if disk_here {
                                ui.separator();
                                if ui.button("Delete").on_hover_text("To the Recycle Bin (asks first; also the Delete key)").clicked() {
                                    menu = Some((*i, Menu::Trash));
                                    ui.close();
                                }
                                ui.separator();
                            }
                            if edit_here {
                                ui.separator();
                                if it.kind != Kind::Dir && ui.button("Replace...").clicked() {
                                    menu = Some((*i, Menu::Replace));
                                    ui.close();
                                }
                                if ui.button("Rename...").clicked() {
                                    menu = Some((*i, Menu::Rename));
                                    ui.close();
                                }
                                if ui.button("Delete").clicked() {
                                    menu = Some((*i, Menu::Delete));
                                    ui.close();
                                }
                                ui.separator();
                            }
                            if ui.button("Copy name").clicked() {
                                menu = Some((*i, Menu::CopyName));
                                ui.close();
                            }
                        });
                    }
                }
            }
        });

        // what was clicked
        if let Some(c) = clicked_col {
            if self.sort == c {
                self.desc = !self.desc;
            } else {
                self.sort = c;
                self.desc = false;
            }
            self.listing = None;
        }
        let item_of = |this: &Self, i: usize| this.listing.as_ref().map(|l| l.items[i].clone());
        if let Some(i) = click {
            self.selected = item_of(self, i).map(|it| it.name);
        }
        // a row dragged: out of the window as a copy (Windows Explorer, the desktop, ...)
        if let Some(it) = drag.and_then(|i| item_of(self, i)) {
            self.selected = Some(it.name.clone());
            let path = self.item_path(&it);
            if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                actions.push(Action::DragOut(n.loc.clone(), it.name.clone(), it.kind));
            }
        }
        if let Some(i) = dbl {
            if let Some(it) = item_of(self, i) {
                self.selected = Some(it.name.clone());
                self.open_item(&it, cx, actions);
            }
        }
        if let Some((i, m)) = menu {
            if let Some(it) = item_of(self, i) {
                self.selected = Some(it.name.clone());
                match m {
                    Menu::Open => self.open_item(&it, cx, actions),
                    Menu::ShowInFolder => {
                        if let Some(h) = self.results.as_ref().and_then(|r| r.iter().find(|h| self.components_of(h).as_ref() == Some(&it.path))).cloned() {
                            self.show_hit(&h);
                        }
                    }
                    Menu::AddTextures | Menu::Extract => {
                        let path = self.item_path(&it);
                        if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                            actions.push(if m == Menu::AddTextures { Action::AddTextures(n.loc.clone(), it.name.clone()) } else { Action::Extract(n.loc.clone(), it.name.clone(), it.kind) });
                        }
                    }
                    Menu::CopyName => ui.ctx().copy_text(it.name.clone()),
                    Menu::Trash => self.delete_selected(cx),
                    Menu::Replace | Menu::Rename | Menu::Delete => {
                        let path = self.item_path(&it);
                        if let Some(place) = self.edit_place(&path, true, cx) {
                            match m {
                                Menu::Replace => {
                                    if let Some(f) = rfd::FileDialog::new().set_title(format!("Replace {} with...", it.name)).pick_file() {
                                        match std::fs::read(&f) {
                                            Ok(data) => self.start_edit(&place, vec![rage_toolkit_core::rpf_edit::Edit::Put { path: place.inner.clone(), data }], format!("Replaced {}", it.name), game),
                                            Err(e) => self.message = format!("{}: {e}", f.display()),
                                        }
                                    }
                                }
                                Menu::Rename => self.renaming = Some((place, it.name.clone())),
                                _ => self.deleting = Some(place),
                            }
                        }
                    }
                }
            }
        }
        let _ = game;
    }

    /// Enter on the selected entry
    fn open_named(&mut self, name: &str, cx: &Ctx, actions: &mut Vec<Action>) {
        if let Some(it) = self.listing.as_ref().and_then(|l| l.items.iter().find(|i| i.name == name)).cloned() {
            self.open_item(&it, cx, actions);
        }
    }

    /// Double-click: go into a folder or archive, look at a model or a texture dictionary.
    fn open_item(&mut self, it: &Item, cx: &Ctx, actions: &mut Vec<Action>) {
        let path = self.item_path(it);
        // in Edit mode, an editor opened on a file of an editable archive saves back into it
        self.opened_place = if self.edit_mode && it.kind == Kind::File { self.edit_place(&path, true, cx) } else { None };
        match it.kind {
            Kind::Dir | Kind::Rpf => self.goto(path),
            Kind::File => {
                if browser::is_picture(&it.name) {
                    if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                        actions.push(Action::Preview(n.loc.clone(), it.name.clone()));
                    }
                    return;
                }
                if browser::ext(&it.name) == "ytyp" {
                    if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                        actions.push(Action::Ytyp(n.loc.clone(), it.name.clone()));
                    }
                    return;
                }
                if browser::is_text(&it.name) {
                    if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                        actions.push(Action::Text(n.loc.clone(), it.name.clone()));
                    }
                    return;
                }
                if browser::ext(&it.name) == "ytd" {
                    if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                        actions.push(Action::Ytd(n.loc.clone(), it.name.clone()));
                    }
                    return;
                }
                if !browser::is_openable(&it.name) {
                    self.message = format!("{}: no viewer for this file type yet (right-click > Extract...)", it.name);
                    return;
                }
                if let Some(n) = browser::find_mut(&mut self.roots, &path, cx) {
                    actions.push(Action::Open(n.loc.clone(), it.name.clone()));
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Trash,
    Open,
    ShowInFolder,
    AddTextures,
    Extract,
    CopyName,
    Replace,
    Rename,
    Delete,
}

// ---- the tree --------------------------------------------------------------------------------------------------

struct TreeCx<'a> {
    current: &'a [String],
    follow: bool,
    go: Option<Vec<String>>,
    remove: Option<String>,
    actions: Vec<Action>,
}

fn tree_node(ui: &mut egui::Ui, node: &mut Node, path: &mut Vec<String>, t: &mut TreeCx, cx: &Ctx) {
    if !node.is_container() {
        return;
    }
    path.push(node.name.clone());
    let is_current = t.current == path.as_slice();
    let on_path = t.follow && t.current.starts_with(path.as_slice());
    let id = ui.make_persistent_id(("explorer_node", path.as_slice()));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    if on_path && !is_current {
        state.set_open(true);
    } else if t.follow && path.len() == 1 && !t.current.starts_with(path.as_slice()) {
        // going somewhere else: fold the other tree (the game's has hundreds of entries)
        state.set_open(false);
    }
    let kind = node.kind;
    let header = state.show_header(ui, |ui| {
        let (irect, _) = ui.allocate_exact_size(egui::vec2(16.0, 14.0), Sense::hover());
        paint_icon(ui.painter(), irect, kind, if kind == Kind::Rpf { 1 } else { 0 });
        let r = ui.selectable_label(is_current, node.shown_name()).on_hover_text(node.where_is());
        if r.clicked() {
            t.go = Some(path.clone());
        }
        r.context_menu(|ui| {
            if ui.button("Extract all...").clicked() {
                t.actions.push(Action::Extract(node.loc.clone(), node.name.clone(), node.kind));
                ui.close();
            }
            if node.removable && ui.button("Remove from the list").clicked() {
                t.remove = Some(node.name.clone());
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(node.where_is());
                ui.close();
            }
        });
        if node.removable && ui.small_button("x").on_hover_text("Remove this folder from the list").clicked() {
            t.remove = Some(node.name.clone());
        }
    });
    header.body(|ui| {
        node.load_if_needed(cx);
        if let Some(e) = node.error() {
            ui.colored_label(crate::theme::err(), e);
        }
        for c in node.children_mut() {
            tree_node(ui, c, path, t, cx);
        }
    });
    path.pop();
}

/// Small icons in the spirit of the classic tools': a yellow folder, a blue archive box, a light-blue model, an orange texture, a grey page.
pub(crate) fn paint_icon(p: &egui::Painter, r: Rect, kind: Kind, rank: u8) {
    let rr = |x: f32, y: f32, w: f32, h: f32, c: Color32| p.rect_filled(Rect::from_min_size(egui::pos2(r.left() + x, r.top() + y), egui::vec2(w, h)), 1.5, c);
    match (kind, rank) {
        (Kind::Dir, _) => {
            rr(0.0, 1.0, 7.0, 3.0, Color32::from_rgb(222, 178, 70));
            rr(0.0, 3.0, 16.0, 11.0, Color32::from_rgb(244, 200, 88));
        }
        (Kind::Rpf, _) => {
            rr(1.0, 0.0, 14.0, 14.0, Color32::from_rgb(70, 130, 215));
            rr(1.0, 0.0, 14.0, 4.0, Color32::from_rgb(120, 176, 245));
            rr(8.0, 4.0, 1.5, 10.0, Color32::from_rgb(50, 100, 180));
        }
        (_, 2) => {
            rr(2.0, 0.0, 12.0, 14.0, Color32::from_rgb(110, 190, 240));
            rr(5.0, 4.0, 6.0, 6.0, Color32::from_rgb(40, 100, 160));
        }
        (_, 3) => {
            rr(2.0, 0.0, 12.0, 14.0, Color32::from_rgb(255, 170, 80));
            rr(4.0, 3.0, 8.0, 3.0, Color32::from_rgb(255, 215, 150));
            rr(4.0, 8.0, 8.0, 3.0, Color32::from_rgb(200, 110, 40));
        }
        (_, 7) => {
            rr(1.0, 1.0, 14.0, 12.0, Color32::from_rgb(150, 165, 190));
            rr(1.0, 1.0, 14.0, 3.0, Color32::from_rgb(70, 110, 190));
        }
        _ => {
            rr(3.0, 0.0, 10.0, 14.0, Color32::from_rgb(190, 192, 198));
            rr(5.0, 4.0, 6.0, 1.5, Color32::from_rgb(120, 122, 130));
            rr(5.0, 7.0, 6.0, 1.5, Color32::from_rgb(120, 122, 130));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, kind: Kind, size: u64) -> Item {
        let (rank, type_name) = browser::type_info(name, kind);
        Item { name: name.into(), kind, size, rank, type_name, place: String::new(), path: vec![] }
    }

    fn names(items: &[Item], rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Group(t) => format!("[{t}]"),
                Row::Item(i) => items[*i].name.clone(),
            })
            .collect()
    }

    #[test]
    fn the_list_is_grouped_by_type_and_sorted_inside_each_group() {
        let items = vec![item("zlib1.dll", Kind::File, 97), item("b", Kind::Dir, 0), item("GTA5.exe", Kind::File, 46_355), item("a", Kind::Dir, 0), item("opus.dll", Kind::File, 397), item("common.rpf", Kind::Rpf, 5)];
        let rows = build_rows(&items, Col::Name, false);
        assert_eq!(names(&items, &rows), ["[Folder  -  2 items]", "a", "b", "[Archive (RPF)  -  1 item]", "common.rpf", "[Application  -  1 item]", "GTA5.exe", "[Dynamic-link library  -  2 items]", "opus.dll", "zlib1.dll"]);
        let by_size = build_rows(&items, Col::Size, true);
        assert_eq!(&names(&items, &by_size)[7..], ["[Dynamic-link library  -  2 items]", "opus.dll", "zlib1.dll"], "biggest first inside the group");
        let by_type = build_rows(&items, Col::Type, false);
        assert_eq!(names(&items, &by_type)[0], "[Application  -  1 item]", "the Type column puts the groups in alphabetical order");
    }

    #[test]
    fn unknown_file_types_share_one_group() {
        let items = vec![item("a.blend", Kind::File, 1), item("b.pur", Kind::File, 2), item("c.blend1", Kind::File, 3)];
        let rows = build_rows(&items, Col::Name, false);
        assert_eq!(names(&items, &rows), ["[Other files  -  3 items]", "a.blend", "b.pur", "c.blend1"]);
        assert_eq!(items[1].type_name, "PUR file", "the Type column still says what each one is");
    }

    #[test]
    fn back_forward_and_up_walk_the_history() {
        let mut h = History::default();
        let (a, b, c) = (vec!["GTA V".to_string()], vec!["GTA V".to_string(), "x64".to_string()], vec!["GTA V".to_string(), "update".to_string()]);
        h.go(&a);
        h.go(&b);
        assert_eq!(h.back(&c), Some(b.clone()));
        assert_eq!(h.back(&b), Some(a.clone()));
        assert_eq!(h.back(&a), None);
        assert_eq!(h.forward(&a), Some(b.clone()));
        h.go(&b);
        assert!(h.fwd.is_empty(), "a new place forgets the forward list");
    }

    #[test]
    fn copy_to_mods_copies_the_game_archive_and_goes_to_the_same_place_in_the_copy() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_ex_mods_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (game_dir, src) = (d.join("GTA V"), d.join("src"));
        std::fs::create_dir_all(src.join("levels")).unwrap();
        std::fs::create_dir_all(&game_dir).unwrap();
        std::fs::write(src.join("levels").join("a.txt"), b"x").unwrap();
        std::fs::write(game_dir.join("x64e.rpf"), rage_toolkit_core::rpf::build(&src).unwrap()).unwrap();
        let mut ex = Explorer::new(None);
        let mut game = Node::folder(&game_dir);
        game.name = "GTA V".into();
        game.removable = false;
        ex.roots = vec![game];
        ex.set_mods(true, Some(mods::default_dir(&game_dir)));
        let cx = Ctx { game: None };
        let at = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        ex.goto(at(&["GTA V", "x64e.rpf", "levels"]));
        ex.rebuild(&cx);
        let (archive, k) = ex.game_archive.clone().expect("inside the game's own archive");
        assert!(ex.editable.is_none(), "the game's own archive is never editable");
        assert_eq!((archive.clone(), k), (game_dir.join("x64e.rpf"), 1));
        ex.start_copy(archive, k);
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        while ex.copy.is_some() && t0.elapsed().as_secs() < 20 {
            ex.poll_copy(&ctx, &cx);
        }
        assert!(game_dir.join("mods").join("x64e.rpf").is_file(), "{}", ex.message);
        assert_eq!(ex.current, at(&["GTA V", "mods", "x64e.rpf", "levels"]), "the same folder, in the copy");
        ex.rebuild(&cx);
        assert!(ex.game_archive.is_none(), "no notice in the mods copy");
        assert_eq!(ex.editable.as_ref().map(|p| (p.chain.len(), p.inner.as_str())), Some((0, "levels")), "the copy can be edited");
        assert_eq!(ex.listing.as_ref().unwrap().items.len(), 1, "the copy lists like the original");
        ex.set_mods(true, Some(d.join("elsewhere")));
        ex.goto(at(&["GTA V", "x64e.rpf"]));
        ex.rebuild(&cx);
        assert!(ex.game_archive.is_some(), "the game's own archive still has the notice");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// wait for the edit thread and take its result into the Explorer
    fn finish_edit(ex: &mut Explorer, cx: &Ctx) {
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        while ex.edit_job.is_some() && t0.elapsed().as_secs() < 20 {
            ex.poll_edit(&ctx, cx);
        }
        ex.rebuild(cx);
    }

    #[test]
    fn edit_mode_adds_renames_and_deletes_inside_a_nested_archive_shown_in_the_tree() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_ex_edit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (inner_src, outer_src, folder) = (d.join("inner"), d.join("outer"), d.join("mine"));
        for p in [inner_src.join("data"), outer_src.join("x"), folder.clone()] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(inner_src.join("data").join("a.meta"), b"<a/>").unwrap();
        std::fs::write(outer_src.join("x").join("inner.rpf"), rage_toolkit_core::rpf::build(&inner_src).unwrap()).unwrap();
        std::fs::write(folder.join("dlc.rpf"), rage_toolkit_core::rpf::build(&outer_src).unwrap()).unwrap();
        let new_file = d.join("added.xml");
        std::fs::write(&new_file, b"<added/>").unwrap();

        let mut ex = Explorer::new(None);
        ex.roots = vec![Node::folder(&folder)];
        let root = ex.roots[0].name.clone();
        let cx = Ctx { game: None };
        let at = |v: &[&str]| std::iter::once(root.clone()).chain(v.iter().map(|s| s.to_string())).collect::<Vec<_>>();
        ex.goto(at(&["dlc.rpf", "x", "inner.rpf", "data"]));
        ex.rebuild(&cx);
        let place = ex.editable.clone().expect("an archive in a folder of mine");
        assert_eq!((place.chain.clone(), place.inner.as_str()), (vec!["x/inner.rpf".to_string()], "data"));
        assert!(ex.take_drop(&[new_file.clone()], None) && ex.edit_job.is_none() && ex.message.contains("Edit mode"), "without Edit mode: a hint, nothing written");
        ex.edit_mode = true;
        assert!(ex.take_drop(&[new_file.clone()], None), "dropped files go into this folder of the archive");
        finish_edit(&mut ex, &cx);
        let names = |ex: &Explorer| ex.listing.as_ref().unwrap().items.iter().map(|i| i.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&ex), ["a.meta", "added.xml"], "{}", ex.message);
        let item = ex.edit_place(&at(&["dlc.rpf", "x", "inner.rpf", "data", "a.meta"]), true, &cx).unwrap();
        ex.start_edit(&item, vec![rage_toolkit_core::rpf_edit::Edit::Rename { path: item.inner.clone(), to: "b.meta".into() }], "rename".into(), None);
        finish_edit(&mut ex, &cx);
        assert_eq!(names(&ex), ["added.xml", "b.meta"], "{}", ex.message);
        let item = ex.edit_place(&at(&["dlc.rpf", "x", "inner.rpf", "data", "added.xml"]), true, &cx).unwrap();
        assert!(ex.save_back(&item, b"<saved/>".to_vec(), None));
        finish_edit(&mut ex, &cx);
        let node = browser::find_mut(&mut ex.roots, &at(&["dlc.rpf", "x", "inner.rpf", "data", "added.xml"]), &cx).unwrap();
        assert_eq!(browser::read(&node.loc).unwrap(), b"<saved/>", "an editor's save lands in the archive");
        ex.deleting = Some(ex.edit_place(&at(&["dlc.rpf", "x", "inner.rpf", "data", "b.meta"]), true, &cx).unwrap());
        let place = ex.deleting.take().unwrap();
        ex.start_edit(&place, vec![rage_toolkit_core::rpf_edit::Edit::Delete { path: place.inner.clone() }], "delete".into(), None);
        finish_edit(&mut ex, &cx);
        assert_eq!(names(&ex), ["added.xml"], "{}", ex.message);
        drop(ex);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn files_dropped_on_a_folder_are_copied_into_it_but_never_into_the_games() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_ex_drop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (mine, from, game_dir) = (d.join("Rescue 5"), d.join("Textures"), d.join("GTA V"));
        for p in [&mine, &from, &game_dir] {
            std::fs::create_dir_all(p).unwrap();
        }
        let mesh = from.join("mesh.dds");
        std::fs::write(&mesh, b"new").unwrap();
        let mut ex = Explorer::new(None);
        let mut game = Node::folder(&game_dir);
        game.name = "GTA V".into();
        game.removable = false;
        ex.roots = vec![game, Node::folder(&mine)];
        ex.set_mods(true, Some(mods::default_dir(&game_dir)));
        let cx = Ctx { game: None };
        let root = ex.roots[1].name.clone();
        ex.goto(vec![root.clone()]);
        ex.rebuild(&cx);
        assert!(ex.take_drop(&[mesh.clone()], None));
        assert_eq!(std::fs::read(mine.join("mesh.dds")).unwrap(), b"new", "{}", ex.message);
        ex.rebuild(&cx);
        assert!(ex.listing.as_ref().unwrap().items.iter().any(|i| i.name == "mesh.dds"), "the folder is listed again");
        // the same name again: nothing is overwritten until Replace
        std::fs::write(&mesh, b"newer").unwrap();
        assert!(ex.take_drop(&[mesh.clone()], None));
        assert_eq!(std::fs::read(mine.join("mesh.dds")).unwrap(), b"new");
        let (dir, files) = ex.replacing.take().expect("asks to replace");
        ex.copy_into(&dir, &files, &cx);
        assert_eq!(std::fs::read(mine.join("mesh.dds")).unwrap(), b"newer");
        // Delete on a folder of mine asks about the selected file (the Recycle Bin itself is not used in tests)
        ex.selected = Some("mesh.dds".into());
        ex.delete_selected(&cx);
        assert_eq!(ex.trashing.take(), Some(mine.join("mesh.dds")));
        // the game's own folder: refused, nothing written
        ex.goto(vec!["GTA V".into()]);
        ex.rebuild(&cx);
        assert!(ex.take_drop(&[mesh.clone()], None) && ex.message.contains("never writes"));
        assert!(!game_dir.join("mesh.dds").exists());
        ex.delete_selected(&cx);
        assert!(ex.trashing.is_none(), "nothing in the game's folder is deleted");
        // its mods folder is fine
        std::fs::create_dir_all(game_dir.join("mods")).unwrap();
        ex.roots[0].reload();
        ex.goto(vec!["GTA V".into(), "mods".into()]);
        ex.rebuild(&cx);
        assert!(ex.take_drop(&[mesh.clone()], None));
        assert!(game_dir.join("mods").join("mesh.dds").is_file(), "{}", ex.message);
        drop(ex);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Puts a scratch file in the real Recycle Bin: run by hand (`cargo test -- --ignored recycle`).
    #[test]
    #[ignore]
    fn recycle_moves_a_file_to_the_recycle_bin() {
        let f = std::env::temp_dir().join(format!("rage_toolkit_recycle_{}.txt", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        recycle(&f).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn search_results_map_to_places_in_the_tree() {
        let mut ex = Explorer::new(None);
        ex.roots = vec![Node::folder(Path::new("D:\\Mods\\Badges"))];
        let mut game = Node::folder(Path::new("E:\\Game"));
        game.name = "GTA V".into();
        game.removable = false;
        ex.roots.insert(0, game);
        let disk = Hit { path: "D:\\Mods\\Badges\\LA\\hand.ytd".into(), size: 1, is_dir: false };
        assert_eq!(ex.components_of(&disk), Some(vec!["D:\\Mods\\Badges".to_string(), "LA".into(), "hand.ytd".into()]));
        let game_hit = Hit { path: "update/x64/dlcpacks/mpheist/dlc.rpf/x64/a.ytd".into(), size: 1, is_dir: false };
        assert_eq!(ex.components_of(&game_hit).unwrap()[..5], ["GTA V", "update", "x64", "dlcpacks", "mpheist"]);
        assert!(ex.components_of(&Hit { path: "C:\\elsewhere\\a.ytd".into(), size: 0, is_dir: false }).is_none());
    }
}

#[cfg(test)]
mod timing {
    use super::*;

    #[test]
    #[ignore = "timing only: cargo test --release -p rage-toolkit-app build_rows_timing -- --ignored --nocapture"]
    fn build_rows_timing() {
        let exts = ["yft", "ytd", "ydr", "meta", "xml", "awc", "ybn", "ymap", "blend", "pur"];
        let items: Vec<Item> = (0..20_000)
            .map(|i| {
                let name = format!("Vehicle_{:05}_{}.{}", (i * 7919) % 20_000, i % 13, exts[i % exts.len()]);
                let (rank, type_name) = browser::type_info(&name, Kind::File);
                Item { name, kind: Kind::File, size: (i * 37 % 1000) as u64, rank, type_name, place: String::new(), path: vec![] }
            })
            .collect();
        for col in [Col::Name, Col::Size, Col::Type] {
            let t = std::time::Instant::now();
            let rows = build_rows(&items, col, false);
            println!("build_rows 20000 items by {col:?}: {:.1} ms ({} rows)", t.elapsed().as_secs_f64() * 1000.0, rows.len());
        }
    }
}

/// Move a file or folder to the Recycle Bin (Windows asks only if it cannot be recycled and would be gone for good).
#[cfg(windows)]
fn recycle(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW};
    let full = std::path::absolute(path).map_err(|e| e.to_string())?;
    // the list of paths ends with two NULs
    let from: Vec<u16> = full.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW { wFunc: FO_DELETE, pFrom: from.as_ptr(), fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as u16, ..Default::default() };
    // SAFETY: `op` and `from` live across the call; no window, no name mappings
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(format!("Windows error {r:#x}"));
    }
    if op.fAnyOperationsAborted != 0 || full.exists() {
        return Err("cancelled".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn recycle(_path: &Path) -> Result<(), String> {
    Err("only on Windows".into())
}
