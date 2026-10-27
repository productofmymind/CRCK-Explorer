use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use rage_toolkit_core::{game, handling, livery, model, pack, pipeline, rpf, siren, texture, validate, ytdtools};
use std::{fs, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(name = "crck-explorer-cli", version, about = "GTA V / FiveM modding toolkit")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List files in an RPF (nested archives included)
    List { rpf: PathBuf },
    /// Unpack an RPF; nested *.rpf become folders
    Extract { rpf: PathBuf, outdir: PathBuf },
    /// Build an RPF7 from a folder; *.rpf subfolders nest
    Rpf { folder: PathBuf, out: PathBuf },
    /// Change an archive in place, like the classic tools' Edit mode: new data is appended, the archive is saved unencrypted (OPEN) and
    /// read back. The game's own archives are refused: edit a copy in the mods folder (`game copy-to-mods`).
    Edit {
        archive: PathBuf,
        /// an archive inside it to edit instead (repeat for deeper ones), e.g. --in levels/gta5/vehicles.rpf
        #[arg(long = "in")]
        inside: Vec<String>,
        #[command(subcommand)]
        op: EditOp,
    },
    /// Build an add-on vehicle pack (sp = dlc.rpf folder tree, fivem = resource)
    Pack { mode: Mode, name: String, srcdir: PathBuf, outdir: PathBuf },
    /// Check a built SP dlc.rpf folder: every content.xml path exists
    Verify { dlc_rpf_dir: PathBuf },
    /// Meta cross-checks for a pack folder (exit 1 on errors)
    Validate {
        packdir: PathBuf,
        /// also check FiveM manifest, stream size limits and folder structure
        #[arg(long)]
        fivem: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Folder of images (png/jpg/tga/bmp/webp/dds) -> one YTD, with automatic format, power-of-two sizing and mips
    Textures {
        folder: PathBuf,
        out: PathBuf,
        /// downscale so the longest side is at most this many pixels
        #[arg(long)]
        max_size: Option<u32>,
        /// non-power-of-two sides: round (default) | down | up | keep
        #[arg(long, default_value = "round")]
        pow2: String,
        /// bc1|bc3|bc4|bc5|bc7|rgba8 for every image (default: automatic per image)
        #[arg(long)]
        format: Option<String>,
        /// warn when the YTD file is bigger than this many MB (FiveM streams up to ~16 MB per file)
        #[arg(long, default_value_t = 16)]
        budget_mb: u64,
    },
    /// List the textures in a YTD (size, format, mips, bytes)
    YtdInfo { ytd: PathBuf },
    /// Inspect a .ydr/.ydd/.yft: LODs (triangles, distances), shaders, textures used; with --ytd also which textures are missing
    ModelInfo {
        model: PathBuf,
        #[arg(long)]
        ytd: Option<PathBuf>,
    },
    /// Export a model as OBJ + MTL, with its textures as PNG (open in any 3D viewer)
    ModelExport {
        model: PathBuf,
        outdir: PathBuf,
        /// texture dictionary that goes with the model
        #[arg(long)]
        ytd: Option<PathBuf>,
        /// high|medium|low|verylow
        #[arg(long, default_value = "high")]
        lod: String,
    },
    /// Read your own installed GTA V (retail encrypted archives; keys come from your GTA5.exe, nothing is shipped)
    Game {
        /// GTA V folder (default: auto-detect; remembered after the first successful `game info`)
        #[arg(long)]
        dir: Option<PathBuf>,
        #[command(subcommand)]
        cmd: GameCmd,
    },
    /// Vehicle livery kits (VMT_LIVERY_MOD)
    Livery {
        #[command(subcommand)]
        cmd: LiveryCmd,
    },
    /// Siren settings in carcols.meta: check them, or work out values
    Siren {
        #[command(subcommand)]
        cmd: SirenCmd,
    },
    /// XML .meta / .xml files: syntax errors with line and column, and the game's rules for the format (the editor's validator)
    Meta {
        #[command(subcommand)]
        cmd: MetaCmd,
    },
    /// .ytyp archetype files: check that CRCK Explorer can read and write them again without losing anything
    Ytyp {
        #[command(subcommand)]
        cmd: YtypCmd,
    },
    /// handling.meta: check values, compare vehicles (your add-on against vanilla with --game)
    Handling {
        #[command(subcommand)]
        cmd: HandlingCmd,
    },
    /// Put a PNG/JPG/... into a YTD (replace by name or add): BCn + mips + optional resize; a DDS goes in as it is
    Png {
        ytd: PathBuf,
        image: PathBuf,
        out: PathBuf,
        /// bc1|bc3|bc4|bc5|bc7|rgba8 (default: automatic)
        #[arg(long)]
        format: Option<String>,
        /// WxH, e.g. 512x512
        #[arg(long)]
        size: Option<String>,
    },
    /// Edit YTDs: merge, split, rename, remove, optimize, report
    Ytd {
        #[command(subcommand)]
        cmd: YtdCmd,
    },
}

#[derive(Subcommand)]
enum YtdCmd {
    /// Combine YTDs into one Legacy YTD (Gen9 inputs are converted)
    Merge {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(short, long)]
        out: PathBuf,
        /// same texture name in two inputs: error | first | last
        #[arg(long, default_value = "error")]
        on_duplicate: String,
    },
    /// Split a YTD into several files in a folder (default: each under 12 MB; FiveM's streaming limit is about 16 MB)
    Split {
        input: PathBuf,
        outdir: PathBuf,
        /// largest file size in MB
        #[arg(long, conflicts_with_all = ["per_texture", "prefix"])]
        max_mb: Option<f64>,
        /// one file per texture
        #[arg(long, conflicts_with = "prefix")]
        per_texture: bool,
        /// one file per first N characters of the texture name
        #[arg(long)]
        prefix: Option<usize>,
    },
    /// Rename textures, case-insensitive: old=new old2=new2 ...
    Rename {
        input: PathBuf,
        out: PathBuf,
        #[arg(required = true)]
        renames: Vec<String>,
    },
    /// Remove textures by name
    Remove {
        input: PathBuf,
        out: PathBuf,
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Shrink: cap the size, re-encode, drop small mips, or squeeze to a total size; never enlarges
    Optimize {
        input: PathBuf,
        out: PathBuf,
        /// downscale textures whose longer side is bigger than this
        #[arg(long)]
        max_size: Option<u32>,
        /// non-power-of-two sides after resizing: round (default) | down | up | keep
        #[arg(long, default_value = "round")]
        pow2: String,
        /// drop mip levels whose longer side is below this many pixels (saves very little)
        #[arg(long)]
        drop_mips_below: Option<u32>,
        /// re-encode every texture to bc1|bc3|bc4|bc5|bc7|rgba8
        #[arg(long)]
        format: Option<String>,
        /// halve the largest textures until the pixel data is at most this many MB
        #[arg(long)]
        target_mb: Option<f64>,
    },
    /// Per texture: size, format, mips, bytes, estimated VRAM and likely problems
    Report { input: PathBuf },
}

#[derive(Subcommand)]
enum MetaCmd {
    /// Check files or folders (every .meta and .xml): exit 1 if any error is found
    Check {
        paths: Vec<PathBuf>,
        /// also show information-level notes
        #[arg(long)]
        notes: bool,
    },
}

#[derive(Subcommand)]
enum YtypCmd {
    /// Read each file, write it again, read that, and compare the two (exit 1 if anything was lost or could not be read)
    Check {
        /// .ytyp files or folders (searched for .ytyp)
        paths: Vec<PathBuf>,
        /// also check every .ytyp in your installed GTA V
        #[arg(long)]
        game: bool,
        /// GTA V folder (default: found automatically)
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// list files that came back identical too
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum GameCmd {
    /// Find the game, derive the archive keys from your own GTA5.exe (cached locally) and remember the folder
    Info,
    /// List files in an archive inside the game, e.g. x64e.rpf/levels/gta5/vehicles.rpf
    Ls { path: String },
    /// Find files by exact name in every game archive (slow the first time: walks all archives)
    Find { name: String },
    /// Search file and/or folder names in every game archive. Words are all required; `*` and `?` make a pattern for the
    /// whole name (`*.ytd`); a word with a `/` is looked for in the whole path (`levels/gta5/vehicles`).
    Search {
        text: String,
        /// what to look at: files, folders (archives count) or both
        #[arg(long, value_enum, default_value = "both")]
        kind: SearchKindArg,
        /// stop after this many matches
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Save one file by its full path (as printed by `find`)
    Get { path: String, out: PathBuf },
    /// Pull a vehicle's models and textures into a folder: the texture dictionaries the game chains to (vehicles.meta), then the shared vehshare*.ytd
    Vehicle { name: String, outdir: PathBuf },
    /// Copy one of the game's own archives (or loose files) to the "mods" folder at the same path, like classic modding tools, e.g.
    /// `update/update.rpf`. An existing copy is left as it is. The game reads it only with the mods-folder ASI plugin and an ASI loader.
    CopyToMods {
        /// path relative to the game folder
        path: PathBuf,
        /// the mods folder (default: <game>\mods)
        #[arg(long)]
        mods: Option<PathBuf>,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum SearchKindArg {
    Files,
    Folders,
    Both,
}

#[derive(Subcommand)]
enum EditOp {
    /// add a file, or replace the one at that path
    Put { path: String, file: PathBuf },
    /// delete a file or a folder
    Rm { path: String },
    /// rename a file or folder (same folder)
    Mv { path: String, to: String },
}

#[derive(Subcommand)]
enum LiveryCmd {
    /// A livery model from a picture: the vehicle's livery file (like polb2_livery1.yft) with the .dds (or image) as its
    /// livery texture, made the same size and format as the template's
    Yft { template: PathBuf, picture: PathBuf, out: PathBuf },
    /// Add a livery to a FiveM vehicle resource: carcols kit entry (append-only), carvariations link, label Lua, manifest lines
    Add {
        pack: PathBuf,
        /// vehicle model name (spawn name)
        #[arg(long)]
        vehicle: String,
        /// livery name; the fragment will be <vehicle>_liv_<name>.yft
        #[arg(long)]
        name: String,
        /// mod-shop text (default: the name)
        #[arg(long)]
        display: Option<String>,
        /// label key (default: the livery model name in capitals)
        #[arg(long)]
        label: Option<String>,
        /// unique kit id; only needed when the vehicle has no livery kit yet
        #[arg(long)]
        kit_id: Option<u32>,
        /// finished livery fragment to copy into stream/
        #[arg(long)]
        yft: Option<PathBuf>,
    },
    /// Show livery kits, their indices, labels, linked vehicles and whether each fragment exists
    List { pack: PathBuf },
    /// Compare two carcols.meta (old = live server, new = your edit): fails if any livery index would shift
    Check { old: PathBuf, new: PathBuf },
}

#[derive(Subcommand)]
enum SirenCmd {
    /// Check the siren settings of a carcols.meta (its carvariations.meta next to it is cross-checked) or of a whole pack folder (exit 1 on errors)
    Check {
        path: PathBuf,
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Calculators: deg2rad <deg> | rad2deg <rad> | seq2dec <binary pattern> | dec2seq <sequencer> | speed <bpm> <sequencer or 0bPATTERN> | start <siren number> <delta deg> [--steady]
    Calc {
        op: String,
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum HandlingCmd {
    /// Duplicate names, missing / non-numeric fields (errors), out-of-range values (warnings); exit 1 on errors
    Check {
        /// handling.meta files or folders (every *handling*.meta below)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Table of chosen fields across files / folders / the game, to compare an add-on vehicle with vanilla ones
    Compare {
        /// handling.meta files or folders
        paths: Vec<PathBuf>,
        /// also list the vanilla vehicles from your own GTA V
        #[arg(long)]
        game: bool,
        /// GTA V folder for --game (default: as `crck-explorer-cli game`)
        #[arg(long)]
        game_dir: Option<PathBuf>,
        /// columns, comma separated: mass, drag, drive-force, top-speed, brake-force, handbrake, traction-max, traction-min, traction-lateral, drive-bias, brake-bias, gears, steering-lock, downforce, model-flags, handling-flags, damage-flags, kind or any field name (default: the first eight, mass ... steering-lock)
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// sort by a column (a short name, a field name, name, kind or source)
        #[arg(long)]
        sort: Option<String>,
        /// largest first
        #[arg(long)]
        desc: bool,
        /// keep vehicles whose name contains any of these, comma separated
        #[arg(long, value_delimiter = ',')]
        filter: Vec<String>,
        /// show only the first N rows (after sorting)
        #[arg(long)]
        limit: Option<usize>,
        /// comma-separated output instead of the aligned table
        #[arg(long)]
        csv: bool,
    },
    /// How many vehicles fall outside the editor's slider ranges and outside our warning ranges (calibration evidence)
    Ranges {
        paths: Vec<PathBuf>,
        #[arg(long)]
        game: bool,
        #[arg(long)]
        game_dir: Option<PathBuf>,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum Mode {
    Sp,
    Fivem,
}

/// True when this program was started by double-clicking it: Windows then gives it a console of its own, which closes the moment
/// it exits. Started from a terminal the console is shared with the shell.
#[cfg(windows)]
fn started_by_double_click() -> bool {
    extern "system" {
        fn GetConsoleProcessList(process_list: *mut u32, process_count: u32) -> u32;
    }
    let mut pids = [0u32; 2];
    // SAFETY: a plain Win32 call with a valid buffer; it returns how many processes are attached to this console (0 when there is none)
    unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 2) == 1 }
}

#[cfg(not(windows))]
fn started_by_double_click() -> bool {
    false
}

/// Start a program that does not share our console (DETACHED_PROCESS), so closing it is not tied to this window.
fn start_detached(exe: &std::path::Path) -> std::io::Result<std::process::Child> {
    let mut c = std::process::Command::new(exe);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0000_0008);
    c.spawn()
}

/// `crck-explorer-cli.exe` is the command line tool, so double-clicking it showed the help for a split second. Open the viewer next to it
/// instead (and say how to use the commands), or keep the window open if the viewer is not there.
fn double_click() -> ExitCode {
    println!("CRCK Explorer: this is the command line tool. For the commands open PowerShell in this folder and run:  .\\crck-explorer-cli.exe --help");
    let viewer = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("crck-explorer.exe"))).filter(|p| p.is_file());
    match viewer.map(|v| start_detached(&v)) {
        // the viewer has no console of its own, so this one closes as soon as we return
        Some(Ok(_)) => {}
        other => {
            if let Some(Err(e)) = other {
                println!("Could not start crck-explorer.exe: {e}");
            } else {
                println!("crck-explorer.exe is not next to this file.");
            }
            println!("Press Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    }
    ExitCode::SUCCESS
}

/// `.ytyp` files under a folder (or the file itself)
fn ytyp_files(path: &std::path::Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut kids: Vec<PathBuf> = fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path()).collect();
        kids.sort();
        for k in kids {
            ytyp_files(&k, out);
        }
    } else if path.extension().map(|e| e.eq_ignore_ascii_case("ytyp")).unwrap_or(false) || path.is_file() && out.is_empty() && path.extension().is_none() {
        out.push(path.to_path_buf());
    }
}

/// `crck-explorer-cli ytyp check`: round-trip every file and report what did not come back.
fn ytyp_check(paths: &[PathBuf], game: bool, game_dir: Option<&std::path::Path>, verbose: bool) -> Result<ExitCode> {
    use rage_toolkit_core::archetype::fidelity::{self, Container};
    // (label, bytes) for each file; the game's files are read one at a time so nothing big is held
    let mut disk = vec![];
    for p in paths {
        if !p.exists() {
            bail!("{} does not exist", p.display());
        }
        ytyp_files(p, &mut disk);
    }
    let g = if game { Some(game::Game::open(game_dir)?) } else { None };
    let in_game: Vec<String> = g.as_ref().map(|g| g.find_where(&|p: &str| p.to_lowercase().ends_with(".ytyp")).into_iter().map(|f| f.path).collect()).unwrap_or_default();
    if disk.is_empty() && in_game.is_empty() {
        bail!("nothing to check: give .ytyp files or folders, or --game");
    }
    let (mut files, mut lossless, mut upgraded, mut lossy, mut failed, mut pso, mut archetypes, mut mlos) = (0, 0, 0, 0, 0, 0, 0, 0);
    let mut one = |label: &str, bytes: std::io::Result<Vec<u8>>, verbose: bool| {
        files += 1;
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => {
                failed += 1;
                println!("FAIL  {label}: {e}");
                return;
            }
        };
        match fidelity::check(&bytes) {
            Err(e) => {
                failed += 1;
                println!("FAIL  {label}: {e:#}");
            }
            Ok(r) => {
                archetypes += r.archetypes;
                mlos += r.mlos;
                if r.container == Container::Pso {
                    pso += 1;
                }
                if r.is_lossless() {
                    lossless += 1;
                    if r.upgraded_fields > 0 {
                        upgraded += 1;
                    }
                    if verbose {
                        println!("ok    {label}  ({} archetypes, {} MLOs{})", r.archetypes, r.mlos, if r.upgraded_fields > 0 { format!(", older layout upgraded: {} fields added", r.upgraded_fields) } else { String::new() });
                    }
                } else {
                    lossy += 1;
                    println!("LOSS  {label}: {} differences", r.total_differences);
                    for d in r.differences.iter().take(5) {
                        println!("        {}  {}  ->  {}", d.path, d.before, d.after);
                    }
                    for w in r.read_warnings.iter().chain(&r.write_warnings).take(3) {
                        println!("        warning: {w}");
                    }
                }
            }
        }
    };
    for p in &disk {
        one(&p.display().to_string(), fs::read(p), verbose);
    }
    if let Some(g) = &g {
        for p in &in_game {
            one(p, g.read(p).map_err(|e| std::io::Error::other(format!("{e:#}"))), verbose);
        }
    }
    println!("{files} files: {lossless} came back identical ({upgraded} of them with an older layout upgraded), {lossy} lost data, {failed} could not be read or written; {pso} were PSO files; {archetypes} archetypes, {mlos} MLOs");
    Ok(if lossy + failed > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// `crck-explorer-cli meta check`: the editor's metadata validator on files.
fn meta_check(paths: &[PathBuf], notes: bool) -> Result<ExitCode> {
    use rage_toolkit_core::metaxml::{self, Severity};
    fn walk(p: &std::path::Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            let mut kids: Vec<PathBuf> = fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
            kids.sort();
            for k in kids {
                walk(&k, out);
            }
        } else if p.extension().map(|e| e.eq_ignore_ascii_case("meta") || e.eq_ignore_ascii_case("xml")).unwrap_or(false) || p.is_file() && p.extension().is_none() {
            out.push(p.to_path_buf());
        }
    }
    let mut files = vec![];
    for p in paths {
        if !p.exists() {
            bail!("{} does not exist", p.display());
        }
        walk(p, &mut files);
    }
    if files.is_empty() {
        bail!("nothing to check: give .meta or .xml files or folders");
    }
    let (mut errors, mut warnings) = (0, 0);
    for f in &files {
        let text = fs::read_to_string(f).map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?;
        let r = metaxml::check(&text);
        for d in r.diagnostics.iter().filter(|d| notes || d.severity != Severity::Info) {
            let tag = match d.severity {
                Severity::Error => {
                    errors += 1;
                    "error"
                }
                Severity::Warning => {
                    warnings += 1;
                    "warning"
                }
                Severity::Info => "note",
            };
            println!("{}:{}:{}: {tag} [{}] {}", f.display(), d.line, d.col, d.code, d.message);
            if !d.fix.is_empty() {
                println!("    fix: {}", d.fix);
            }
        }
    }
    println!("{} file(s): {errors} error(s), {warnings} warning(s)", files.len());
    Ok(if errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn main() -> ExitCode {
    if std::env::args_os().len() == 1 && started_by_double_click() {
        return double_click();
    }
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::List { rpf } => {
            for (p, n) in rpf::list(&rpf)? {
                println!("{p}  {n}");
            }
        }
        Cmd::Edit { archive, inside, op } => {
            use rage_toolkit_core::rpf_edit::{edit, Edit};
            let e = match op {
                EditOp::Put { path, file } => Edit::Put { path, data: fs::read(&file)? },
                EditOp::Rm { path } => Edit::Delete { path },
                EditOp::Mv { path, to } => Edit::Rename { path, to },
            };
            // the game, when found: its keys read a fresh (encrypted) copy, and its own archives are refused
            let g = game::Game::open(None).ok();
            let keys = g.as_ref().map(|g| g.keys());
            let guard = g.as_ref().map(|g| (g.dir.clone(), rage_toolkit_core::mods::default_dir(&g.dir)));
            let r = edit(&archive, &inside, &[e], keys.as_deref(), guard.as_ref().map(|(a, b)| (a.as_path(), b.as_path())))?;
            println!("{}", r.summary());
        }
        Cmd::Extract { rpf, outdir } => println!("extracted {} files to {}", rpf::extract(&rpf, &outdir)?, outdir.display()),
        Cmd::Rpf { folder, out } => {
            let bytes = rpf::build(&folder)?;
            fs::write(&out, &bytes)?;
            println!("built {} ({} bytes)", out.display(), bytes.len());
        }
        Cmd::Pack { mode, name, srcdir, outdir } => {
            pack::build(if matches!(mode, Mode::Sp) { pack::Mode::Sp } else { pack::Mode::Fivem }, &name, &srcdir, &outdir)?;
        }
        Cmd::Verify { dlc_rpf_dir } => return Ok(if pack::verify(&dlc_rpf_dir)? == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) }),
        Cmd::Validate { packdir, fivem, json } => {
            let r = validate::run(&packdir, if fivem { validate::Profile::Fivem } else { validate::Profile::Core });
            if json {
                print!("{}", validate::to_json(&r));
            } else {
                validate::print(&r);
            }
            return Ok(if r.errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS });
        }
        Cmd::Textures { folder, out, max_size, pow2, format, budget_mb } => {
            let pow2 = match pow2.as_str() {
                "round" => pipeline::Pow2::Round,
                "down" => pipeline::Pow2::Down,
                "up" => pipeline::Pow2::Up,
                "keep" => pipeline::Pow2::Keep,
                o => bail!("--pow2 must be round|down|up|keep, got '{o}'"),
            };
            let format = format.as_deref().map(rage_formats_parse).transpose()?;
            let rep = pipeline::folder_to_ytd(&folder, &out, &pipeline::Options { max_size, pow2, format })?;
            print_report(&rep);
            let size = fs::metadata(&out)?.len();
            println!("{} texture(s) -> {} ({:.2} MB)", rep.len(), out.display(), size as f64 / 1048576.0);
            if size > budget_mb * 1048576 {
                eprintln!("warning: {:.1} MB is over the {budget_mb} MB budget; try --max-size 1024 or a smaller --pow2 down", size as f64 / 1048576.0);
            }
        }
        Cmd::Game { dir, cmd } => {
            let g = game::Game::open(dir.as_deref())?;
            match cmd {
                GameCmd::Info => {
                    game::save_game_dir(&g.dir)?;
                    let archives = g.top_level_archives();
                    println!("game: {}\nkeys: derived from GTA5.exe and cached\n{} top-level archives (e.g. {})", g.dir.display(), archives.len(), archives.iter().take(4).cloned().collect::<Vec<_>>().join(", "));
                }
                GameCmd::Ls { path } => {
                    let ar = g.archive(&path)?;
                    for f in ar.files() {
                        println!("{}  {}", f.path, f.size);
                    }
                }
                GameCmd::Find { name } => {
                    let r = g.find(&name);
                    if r.is_empty() {
                        println!("not found");
                    }
                    for f in r {
                        println!("{}  {}", f.path, f.size);
                    }
                }
                GameCmd::Search { text, kind, limit } => {
                    use rage_toolkit_core::search::{Kind, Query};
                    let kind = match kind {
                        SearchKindArg::Files => Kind::Files,
                        SearchKindArg::Folders => Kind::Folders,
                        SearchKindArg::Both => Kind::Both,
                    };
                    let Some(q) = Query::parse(&text, kind) else { bail!("nothing to search for") };
                    let hits = g.search(&q, limit);
                    for h in &hits {
                        if h.is_dir {
                            println!("{}/", h.path);
                        } else {
                            println!("{}  {}", h.path, h.size);
                        }
                    }
                    println!("{} match(es){}", hits.len(), if hits.len() >= limit { " (limit reached: raise --limit)" } else { "" });
                }
                GameCmd::Get { path, out } => {
                    let data = g.read(&path)?;
                    fs::write(&out, &data)?;
                    println!("{} bytes -> {}", data.len(), out.display());
                }
                GameCmd::Vehicle { name, outdir } => {
                    for p in g.extract_vehicle(&name, &outdir)? {
                        println!("{}", p.display());
                    }
                }
                GameCmd::CopyToMods { path, mods } => {
                    let mods = mods.unwrap_or_else(|| rage_toolkit_core::mods::default_dir(&g.dir));
                    let src = if path.is_absolute() { path } else { g.dir.join(path) };
                    let mut last = 0;
                    let r = rage_toolkit_core::mods::copy_to_mods(&g.dir, &mods, &src, &mut |done, total| {
                        let pct = (done * 100).checked_div(total).unwrap_or(100);
                        if pct >= last + 10 {
                            eprintln!("{pct}%");
                            last = pct;
                        }
                        true
                    })?;
                    match r {
                        rage_toolkit_core::mods::Copied::New(p) => println!("copied to {}", p.display()),
                        rage_toolkit_core::mods::Copied::Existing(p) => println!("{} exists already: left as it is", p.display()),
                    }
                }
            }
        }
        Cmd::Livery { cmd } => match cmd {
            LiveryCmd::Add { pack, vehicle, name, display, label, kit_id, yft } => {
                let log = livery::add(&livery::AddParams { pack: &pack, vehicle: &vehicle, name: &name, display: display.as_deref(), label: label.as_deref(), kit_id, yft: yft.as_deref() })?;
                for l in log {
                    println!("{l}");
                }
            }
            LiveryCmd::Yft { template, picture, out } => println!("{}", rage_toolkit_core::livery_model::livery_yft(&template, &picture, &out)?),
            LiveryCmd::List { pack } => {
                for k in livery::list(&pack)? {
                    println!("{} (id {}, {}) -> vehicles: {}", k.kit, k.id, k.kit_type, if k.linked_to.is_empty() { "none (not linked in carvariations.meta)".to_string() } else { k.linked_to.join(", ") });
                    for (i, model, label, yft) in k.liveries {
                        println!("  #{i}  {model}  label {label}  {}", if yft { "yft ok" } else { "yft MISSING" });
                    }
                }
            }
            LiveryCmd::Check { old, new } => {
                let bad = livery::append_only(&fs::read_to_string(&old)?, &fs::read_to_string(&new)?)?;
                if bad.is_empty() {
                    println!("ok: only appends; existing livery indices are unchanged");
                } else {
                    for b in &bad {
                        println!("[E] {b}");
                    }
                    return Ok(ExitCode::from(1));
                }
            }
        },
        Cmd::Meta { cmd: MetaCmd::Check { paths, notes } } => return meta_check(&paths, notes),
        Cmd::Ytyp { cmd: YtypCmd::Check { paths, game, game_dir, verbose } } => return ytyp_check(&paths, game, game_dir.as_deref(), verbose),
        Cmd::Siren { cmd } => match cmd {
            SirenCmd::Check { path, json } => {
                let r = siren::check(&path)?;
                if json {
                    print!("{}", validate::to_json(&r));
                } else {
                    validate::print(&r);
                }
                return Ok(if r.errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS });
            }
            SirenCmd::Calc { op, args } => println!("{}", siren::calc(&op, &args)?),
        },
        Cmd::ModelInfo { model: path, ytd } => {
            let reps = model::inspect(&path)?;
            for r in &reps {
                println!("{}  size {:.2} x {:.2} x {:.2}", r.name, r.size[0], r.size[1], r.size[2]);
                for l in &r.lods {
                    println!("  {:<8} to {:>7.1}  {} model(s) {} geometr(ies) {:>7} verts {:>7} tris", l.level, l.distance, l.models, l.geometries, l.vertices, l.triangles);
                }
                for sh in &r.shaders {
                    println!("  shader #{} {}: {}", sh.id, sh.name, sh.textures.iter().map(|(p, t)| format!("{p}={t}")).collect::<Vec<_>>().join(", "));
                }
                if !r.embedded_textures.is_empty() {
                    println!("  embedded textures: {}", r.embedded_textures.join(", "));
                }
            }
            if let Some(y) = ytd {
                let missing = model::missing_textures(&reps, &model::ytd_names(&y)?);
                if missing.is_empty() {
                    println!("textures: every texture the shaders name is in the YTD or embedded");
                } else {
                    println!("textures: {} named by shaders but not in the YTD or embedded: {}", missing.len(), missing.join(", "));
                }
            }
        }
        Cmd::ModelExport { model: path, outdir, ytd, lod } => {
            let level: rage_toolkit_core::model::LodLevel = lod.parse()?;
            println!("{}", model::export_obj(&path, &outdir, ytd.as_deref(), level)?);
        }
        Cmd::YtdInfo { ytd } => {
            let (rep, edition) = pipeline::ytd_info(&ytd)?;
            if let Some(e) = edition {
                println!("edition: {e}{}", if e == rage_toolkit_core::Edition::Gen9 { " (shown as converted to Legacy)" } else { "" });
            }
            print_report(&rep);
        }
        Cmd::Handling { cmd } => return handling_cmd(cmd),
        Cmd::Png { ytd, image, out, format, size } => {
            let fmt = match format {
                Some(f) => Some(rage_formats_parse(&f)?),
                None => None,
            };
            let size = match size {
                Some(s) => {
                    let (w, h) = s.split_once(['x', 'X']).ok_or_else(|| anyhow::anyhow!("size must be WxH"))?;
                    Some((w.parse()?, h.parse()?))
                }
                None => None,
            };
            let r = texture::import_image(&ytd, &image, &out, fmt, size)?;
            if let Some(w) = &r.warning {
                eprintln!("warning: {w}");
            }
            println!("{} {}; {}x{}", if r.replaced { "replaced" } else { "added" }, r.name, r.width, r.height);
        }
        Cmd::Ytd { cmd } => ytd_cmd(cmd)?,
    }
    Ok(ExitCode::SUCCESS)
}

fn ytd_cmd(cmd: YtdCmd) -> Result<()> {
    use ytdtools::{OnDuplicate, OptimizeOptions, SplitBy};
    let mb = |n: u64| n as f64 / 1048576.0;
    match cmd {
        YtdCmd::Merge { inputs, out, on_duplicate } => {
            let policy = match on_duplicate.as_str() {
                "error" => OnDuplicate::Error,
                "first" => OnDuplicate::KeepFirst,
                "last" => OnDuplicate::KeepLast,
                o => bail!("--on-duplicate must be error|first|last, got '{o}'"),
            };
            let paths: Vec<&std::path::Path> = inputs.iter().map(|p| p.as_path()).collect();
            let m = ytdtools::merge(&paths, &out, policy)?;
            for d in &m.decisions {
                println!("duplicate '{}': kept the one from {}, dropped the one from {}", d.name, d.kept.display(), d.dropped.display());
            }
            println!("{} texture(s) -> {} ({:.2} MB)", m.textures, out.display(), mb(m.bytes));
        }
        YtdCmd::Split { input, outdir, max_mb, per_texture, prefix } => {
            let by = match (per_texture, prefix) {
                (true, _) => SplitBy::PerTexture,
                (_, Some(n)) => SplitBy::Prefix(n),
                _ => SplitBy::MaxSize(max_mb.map_or(ytdtools::DEFAULT_SPLIT_BYTES, |m| (m * 1048576.0) as u64)),
            };
            for p in ytdtools::split(&input, &outdir, by)? {
                println!("{}  {:.2} MB  {} texture(s): {}", p.path.display(), mb(p.bytes), p.textures.len(), p.textures.join(", "));
                if p.over_limit {
                    eprintln!("warning: {} is over the limit; it is a single texture, shrink it with `crck-explorer-cli ytd optimize`", p.path.display());
                }
            }
        }
        YtdCmd::Rename { input, out, renames } => {
            let pairs = renames.iter().map(|r| r.split_once('=').ok_or_else(|| anyhow::anyhow!("'{r}' is not old=new"))).collect::<Result<Vec<_>>>()?;
            for (old, new) in ytdtools::rename(&input, &out, &pairs)? {
                println!("{old} -> {new}");
            }
        }
        YtdCmd::Remove { input, out, names } => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            for n in ytdtools::remove(&input, &out, &names)? {
                println!("removed {n}");
            }
        }
        YtdCmd::Optimize { input, out, max_size, pow2, drop_mips_below, format, target_mb } => {
            let opts = OptimizeOptions {
                max_size,
                pow2: ytdtools::parse_pow2(&pow2)?,
                drop_mips_below,
                format_override: format.as_deref().map(rage_formats_parse).transpose()?,
                target_bytes: target_mb.map(|m| (m * 1048576.0) as u64),
            };
            let r = ytdtools::optimize(&input, &out, &opts)?;
            print!("{}", ytdtools::format_optimize(&r));
            if r.target_met == Some(false) {
                eprintln!("warning: could not reach the target; the largest textures are already as small as --target-mb allows (sides stop at 32 px)");
            }
        }
        YtdCmd::Report { input } => print!("{}", ytdtools::format_report(&ytdtools::report(&input)?)),
    }
    Ok(())
}

fn handling_cmd(cmd: HandlingCmd) -> Result<ExitCode> {
    match cmd {
        HandlingCmd::Check { paths, json } => {
            let r = handling::check(&paths)?;
            if json {
                print!("{}", validate::to_json(&r));
            } else {
                validate::print(&r);
            }
            return Ok(if r.errors == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) });
        }
        HandlingCmd::Compare { paths, game, game_dir, fields, sort, desc, filter, limit, csv } => {
            let t = handling::compare(&handling::CompareOptions { paths, game, game_dir, fields, sort, desc, filter, limit })?;
            print!("{}", if csv { t.csv() } else { t.render() });
        }
        HandlingCmd::Ranges { paths, game, game_dir } => print!("{}", handling::render_ranges(&handling::ranges(&handling::load(&paths, game, game_dir.as_deref())?))),
    }
    Ok(ExitCode::SUCCESS)
}

fn rage_formats_parse(s: &str) -> Result<texture::EncodeFormat> {
    match texture::EncodeFormat::parse(s) {
        Some(f) => Ok(f),
        None => bail!("unknown format '{s}' (bc1|bc3|bc4|bc5|bc7|rgba8)"),
    }
}

fn print_report(rep: &[pipeline::TextureReport]) {
    let w = rep.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    println!("{:<w$}  {:>9}  {:<8} {:>4}  {:>10}  note", "name", "size", "format", "mips", "bytes");
    for r in rep {
        println!("{:<w$}  {:>9}  {:<8} {:>4}  {:>10}  {}", r.name, format!("{}x{}", r.width, r.height), r.format, r.levels, r.bytes, r.note.as_deref().unwrap_or(""));
    }
}
