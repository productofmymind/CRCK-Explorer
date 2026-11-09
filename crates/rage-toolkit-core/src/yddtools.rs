//! Textures embedded in models (`.ydd` drawable dictionaries such as ped clothing and props, and single `.ydr`
//! drawables): list them, shrink them with the planner `ytd optimize` uses (cap the size, re-encode, drop small mips,
//! halve the biggest until a budget fits), replace or export one, and do it for a whole folder.
//!
//! Only the textures picked by format and name are touched; every other texture and everything that is not a
//! texture is written back as read. Each written file is read back twice, by the block reader and by the older,
//! independent `parse_drawables`: the drawables, their models, geometries, vertex and index data, shaders and skeletons
//! must match the input exactly, and the textures must be what was planned. A file that fails that is not written.
//! A `.yft` (fragment) is not handled: its embedded textures cannot be written back yet.
use crate::texture::{replacement_for, ReplaceOpts};
use crate::ytdtools::{optimize_textures, OptimizeOptions, TextureChange};
use anyhow::{bail, Context, Result};
use rage_formats::{
    drawable_embedded_textures, embedded_textures, parse_drawables, read_ydd, read_ydr, replace_texture, texture_of, write_ydd, write_ydr,
    gen9::Edition,
    texture_encode::EncodeFormat,
    ydd::{DrawableEntry, DrawableKind, ShaderParameterValue},
    ytd::{mip_chain_size, TextureFormat, YtdTexture},
    BlockId, EmbeddedTexture, Graph,
};
use std::{
    collections::hash_map::DefaultHasher,
    fmt::Write as _,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

// ---------------------------------------------------------------- models

/// The model files whose embedded textures can be rewritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    Ydd,
    Ydr,
}

/// The kind of `path` by its extension; a `.yft` or anything else is an error that says why.
pub fn model_kind(path: &Path) -> Result<ModelKind> {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("ydd") => Ok(ModelKind::Ydd),
        Some("ydr") => Ok(ModelKind::Ydr),
        Some("yft") => bail!("{}: textures embedded in a .yft cannot be written back yet (only .ydd and .ydr)", path.display()),
        _ => bail!("{}: not a .ydd or .ydr model", path.display()),
    }
}

/// Whether `path` is a model this module can rewrite.
pub fn is_model(path: &Path) -> bool {
    model_kind(path).is_ok()
}

struct Model {
    kind: ModelKind,
    g: Graph,
    root: BlockId,
}

impl Model {
    fn read(file: &[u8], kind: ModelKind) -> Result<Model> {
        let (g, root) = match kind {
            ModelKind::Ydd => read_ydd(file)?,
            ModelKind::Ydr => {
                match Edition::of_file("ydr", file)? {
                    Some(Edition::Legacy) => {}
                    Some(Edition::Gen9) => bail!("this .ydr is a Gen9 (Enhanced) resource; only legacy (version 165) drawables can be rewritten"),
                    None => bail!("not a drawable of a known version (legacy .ydr files are version 165)"),
                }
                read_ydr(file)?
            }
        };
        Ok(Model { kind, g, root })
    }

    fn textures(&self) -> Vec<EmbeddedTexture> {
        match self.kind {
            ModelKind::Ydd => embedded_textures(&self.g, self.root),
            ModelKind::Ydr => drawable_embedded_textures(&self.g, self.root),
        }
    }

    fn write(&mut self) -> Result<Vec<u8>> {
        match self.kind {
            ModelKind::Ydd => write_ydd(&mut self.g, self.root),
            ModelKind::Ydr => write_ydr(&mut self.g, self.root),
        }
    }
}

fn read_model(input: &Path) -> Result<(Vec<u8>, Model)> {
    let kind = model_kind(input)?;
    let file = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let m = Model::read(&file, kind).with_context(|| format!("{}", input.display()))?;
    Ok((file, m))
}

fn write_file(out: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(out, bytes).with_context(|| format!("writing {}", out.display()))
}

// ---------------------------------------------------------------- report

/// One embedded texture.
#[derive(Debug, Clone)]
pub struct YddTextureRow {
    /// index of the first drawable holding it, its name hash and its own name
    pub drawable_index: usize,
    pub drawable_hash: u32,
    pub drawable_name: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub levels: u8,
    /// pixel data as stored
    pub bytes: usize,
    /// the whole mip chain with each level rounded up to whole blocks (no driver padding)
    pub vram: usize,
    /// `Some(stored)` when the file's header has a stride that is wrong for the format (common for ATI2 written by
    /// some tools: half the real one); the pixels were read by the right stride, which a stride-trusting reader does
    /// not do, so such a texture decodes as garbage elsewhere
    pub wrong_stride: Option<u16>,
}

fn row(e: &EmbeddedTexture, t: &YtdTexture) -> YddTextureRow {
    YddTextureRow {
        wrong_stride: (e.stored_stride != t.stride).then_some(e.stored_stride),
        drawable_index: e.drawable_index,
        drawable_hash: e.drawable_hash,
        drawable_name: e.drawable_name.clone(),
        name: t.name.clone(),
        width: t.width as u32,
        height: t.height as u32,
        format: t.format,
        levels: t.levels,
        bytes: t.pixel_data.len(),
        vram: mip_chain_size(t.format, t.width, t.height, t.levels),
    }
}

/// Every embedded texture of a `.ydd` or `.ydr`, in drawable order.
pub fn report(input: &Path) -> Result<Vec<YddTextureRow>> {
    let (_, m) = read_model(input)?;
    Ok(m.textures().iter().map(|e| row(e, &texture_of(&m.g, e.texture))).collect())
}

fn drawable_label(index: usize, hash: u32, name: &str) -> String {
    if name.is_empty() { format!("#{index} 0x{hash:08X}") } else { format!("#{index} {name}") }
}

pub fn format_report(path: &Path, rows: &[YddTextureRow]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{}", path.display());
    if rows.is_empty() {
        let _ = writeln!(s, "  no embedded textures");
        return s;
    }
    let dw = rows.iter().map(|r| drawable_label(r.drawable_index, r.drawable_hash, &r.drawable_name).len()).max().unwrap_or(8).max(8);
    let nw = rows.iter().map(|r| r.name.len()).max().unwrap_or(7).max(7);
    let _ = writeln!(s, "  {:<dw$}  {:<nw$}  {:>9}  {:<8} {:>4}  {:>10}  {:>10}", "drawable", "texture", "size", "format", "mips", "bytes", "vram~");
    for r in rows {
        let mut ati2 = if r.format == TextureFormat::ATI2 { "  ATI2".to_string() } else { String::new() };
        if let Some(st) = r.wrong_stride {
            let _ = write!(ati2, "  (header stride {st} is wrong for {} {}x{}; read with the right one)", r.format, r.width, r.height);
        }
        let _ = writeln!(
            s,
            "  {:<dw$}  {:<nw$}  {:>9}  {:<8} {:>4}  {:>10}  {:>10}{ati2}",
            drawable_label(r.drawable_index, r.drawable_hash, &r.drawable_name),
            r.name,
            format!("{}x{}", r.width, r.height),
            r.format.to_string(),
            r.levels,
            r.bytes,
            r.vram
        );
    }
    let ati2: Vec<&YddTextureRow> = rows.iter().filter(|r| r.format == TextureFormat::ATI2).collect();
    let wrong = rows.iter().filter(|r| r.wrong_stride.is_some()).count();
    let _ = writeln!(
        s,
        "  {} embedded texture(s), {} bytes stored, ~{} bytes VRAM; {} ATI2 ({} bytes){}",
        rows.len(),
        rows.iter().map(|r| r.bytes).sum::<usize>(),
        rows.iter().map(|r| r.vram).sum::<usize>(),
        ati2.len(),
        ati2.iter().map(|r| r.bytes).sum::<usize>(),
        if wrong > 0 { format!("; {wrong} with a wrong header stride") } else { String::new() }
    );
    s
}

// ---------------------------------------------------------------- optimize

#[derive(Clone, Debug)]
pub struct YddOptimizeOptions {
    /// size cap, re-encode format, mip dropping and the byte budget, as for `ytd optimize`. A `target_bytes`
    /// budget is for the embedded pixel data of one model.
    pub base: OptimizeOptions,
    /// only textures stored in one of these formats are touched (`ati2`, `dxt5`, ...); empty for all
    pub only_formats: Vec<EncodeFormat>,
    /// only textures whose name contains one of these (case-insensitive); empty for all
    pub name_contains: Vec<String>,
    /// only textures with exactly one of these names (case-insensitive); empty for all
    pub exact_names: Vec<String>,
    /// for a folder: also write the models where nothing changed (a full copy of the folder), or only the changed ones
    pub write_unchanged: bool,
}

impl Default for YddOptimizeOptions {
    fn default() -> Self {
        Self { base: OptimizeOptions::default(), only_formats: vec![], name_contains: vec![], exact_names: vec![], write_unchanged: true }
    }
}

/// Parses `--only` values: the BC names and the game's (`dxt1`, `dxt5`, `ati2`, `bc7`...).
pub fn parse_formats(list: &str) -> Result<Vec<EncodeFormat>> {
    list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| EncodeFormat::parse(s).with_context(|| format!("unknown texture format '{s}' (try ati2, dxt1, dxt5, bc7)"))).collect()
}

/// Whether a texture stored as `format` is one of `only` (always, for an empty list). DXT3 and DXT5 both count as
/// `dxt5` / `bc3`.
pub fn matches_formats(format: TextureFormat, only: &[EncodeFormat]) -> bool {
    only.is_empty() || crate::ytdtools::encode_format_of(format).is_some_and(|f| only.contains(&f))
}

fn picks(name: &str, format: TextureFormat, opts: &YddOptimizeOptions) -> bool {
    let name = name.to_lowercase();
    matches_formats(format, &opts.only_formats)
        && (opts.name_contains.is_empty() || opts.name_contains.iter().any(|n| name.contains(&n.to_lowercase())))
        && (opts.exact_names.is_empty() || opts.exact_names.iter().any(|n| n.eq_ignore_ascii_case(&name)))
}

#[derive(Debug)]
pub struct YddOptimized {
    pub input: PathBuf,
    pub output: PathBuf,
    /// per embedded texture: where it is, whether it was picked, what changed
    pub textures: Vec<(YddTextureRow, bool, TextureChange)>,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub file_before: u64,
    pub file_after: u64,
    /// `Some` when a target was asked for: whether the embedded pixel data fits it
    pub target_met: Option<bool>,
    /// nothing changed: the input was copied as is, not rebuilt
    pub copied: bool,
    /// nothing changed and nothing was written (a folder run with `write_unchanged` off)
    pub skipped: bool,
}

impl YddOptimized {
    /// How many textures changed.
    pub fn changed(&self) -> usize {
        self.textures.iter().filter(|t| t.2.changed()).count()
    }
}

/// The work of [`optimize`]: the new file's bytes (the input's when nothing changed) and what was done.
fn optimize_bytes(input: &Path, file: &[u8], mut m: Model, opts: &YddOptimizeOptions) -> Result<(Vec<u8>, YddOptimized)> {
    let list = m.textures();
    let src: Vec<YtdTexture> = list.iter().map(|e| texture_of(&m.g, e.texture)).collect();
    let picked: Vec<bool> = src.iter().map(|t| picks(&t.name, t.format, opts)).collect();
    let (new, changes) = optimize_textures(&src, &opts.base, &picked)?;

    let changed: Vec<usize> = (0..src.len()).filter(|&i| changes[i].changed()).collect();
    let (bytes, copied) = if changed.is_empty() {
        (file.to_vec(), true)
    } else {
        for &i in &changed {
            replace_texture(&mut m.g, list[i].texture, &new[i]).with_context(|| format!("replacing '{}'", src[i].name))?;
        }
        let kind = m.kind;
        let bytes = m.write().with_context(|| format!("writing {}", input.display()))?;
        verify(kind, file, &bytes, &new).with_context(|| format!("{}: the rebuilt file did not check out, nothing was written", input.display()))?;
        (bytes, false)
    };
    let bytes_before = src.iter().map(|t| t.pixel_data.len()).sum();
    let bytes_after = new.iter().map(|t| t.pixel_data.len()).sum::<usize>();
    let report = YddOptimized {
        input: input.to_path_buf(),
        output: PathBuf::new(),
        textures: list.iter().zip(&src).zip(picked).zip(changes).map(|(((e, t), p), c)| (row(e, t), p, c)).collect(),
        bytes_before,
        bytes_after,
        file_before: file.len() as u64,
        file_after: bytes.len() as u64,
        target_met: opts.base.target_bytes.map(|tb| bytes_after as u64 <= tb),
        copied,
        skipped: false,
    };
    Ok((bytes, report))
}

/// Shrinks the embedded textures of one `.ydd` / `.ydr` and writes the result to `out` (which must not be `input`).
pub fn optimize(input: &Path, out: &Path, opts: &YddOptimizeOptions) -> Result<YddOptimized> {
    if same_file(input, out) {
        bail!("{}: write the result to a new file, not over the input", input.display());
    }
    let (file, m) = read_model(input)?;
    let (bytes, mut r) = optimize_bytes(input, &file, m, opts)?;
    if r.copied && !opts.write_unchanged {
        r.skipped = true;
    } else {
        write_file(out, &bytes)?;
        r.output = out.to_path_buf();
    }
    Ok(r)
}

/// Every `.ydd` and `.ydr` under `input` (recursively) to the same relative path under `out`. `progress` is called
/// before each file with (files done, files in all, the file).
pub fn optimize_dir(input: &Path, out: &Path, opts: &YddOptimizeOptions, progress: &dyn Fn(usize, usize, &Path)) -> Result<Vec<Result<YddOptimized>>> {
    if same_file(input, out) {
        bail!("{}: write the results to another folder, not over the input", input.display());
    }
    let files = models_in(input, Some(out))?;
    let n = files.len();
    Ok(files
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            progress(i, n, &f);
            let rel = f.strip_prefix(input).unwrap_or(&f).to_path_buf();
            optimize(&f, &out.join(rel), opts)
        })
        .collect())
}

/// The `.ydd` and `.ydr` files under `dir`, sorted, leaving out anything inside `skip` (an output folder inside it).
pub fn models_in(dir: &Path, skip: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_models(dir, &mut files)?;
    files.sort();
    let skip = skip.and_then(|s| fs::canonicalize(s).ok());
    Ok(files.into_iter().filter(|f| skip.as_ref().is_none_or(|o| !fs::canonicalize(f).is_ok_and(|f| f.starts_with(o)))).collect())
}

fn collect_models(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = entry?.path();
        if p.is_dir() {
            collect_models(&p, out)?;
        } else if is_model(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn format_optimize(o: &YddOptimized) -> String {
    let mut s = String::new();
    if o.skipped {
        let _ = writeln!(s, "{}: nothing to change, not written", o.input.display());
        return s;
    }
    let _ = writeln!(s, "{} -> {}", o.input.display(), o.output.display());
    let nw = o.textures.iter().map(|(r, _, _)| r.name.len()).max().unwrap_or(4).max(4);
    for (r, picked, c) in &o.textures {
        let (b, a) = (&c.before, &c.after);
        let at = drawable_label(r.drawable_index, r.drawable_hash, &r.drawable_name);
        if c.changed() {
            let _ = writeln!(s, "  {:<nw$}  {}x{} {} {}mip {} B  ->  {}x{} {} {}mip {} B  {}  [{at}]", b.name, b.width, b.height, b.format, b.levels, b.bytes, a.width, a.height, a.format, a.levels, a.bytes, a.note.as_deref().unwrap_or(""));
        } else {
            let why = if *picked { a.note.clone().unwrap_or_else(|| "nothing to do".into()) } else { "not picked".into() };
            let _ = writeln!(s, "  {:<nw$}  unchanged ({}x{} {} {}mip {} B): {why}  [{at}]", b.name, b.width, b.height, b.format, b.levels, b.bytes);
        }
    }
    let met = match o.target_met {
        Some(true) => "; target met",
        Some(false) => "; TARGET NOT MET (nothing more could shrink)",
        None => "",
    };
    let how = if o.copied { "; nothing changed, copied as is" } else { "" };
    let _ = writeln!(s, "  embedded pixel data {} -> {} bytes; file {} -> {} bytes{met}{how}", o.bytes_before, o.bytes_after, o.file_before, o.file_after);
    s
}

// ---------------------------------------------------------------- scan (what a folder run would pick)

/// What a folder holds and what [`optimize_dir`] with `opts` would pick: counted from the headers, nothing is encoded.
#[derive(Debug, Default)]
pub struct Scan {
    pub files: usize,
    pub textures: usize,
    pub embedded_bytes: usize,
    /// picked by format and name, and bigger than the size cap (when one is set)
    pub to_shrink: usize,
    pub to_shrink_bytes: usize,
    /// rough size of those after the cap (a quarter per halving)
    pub to_shrink_after: usize,
    /// models with at least one texture to shrink, with how many
    pub per_file: Vec<(PathBuf, usize, usize)>,
    pub wrong_stride: usize,
    pub errors: Vec<String>,
}

pub fn scan_dir(input: &Path, opts: &YddOptimizeOptions, progress: &dyn Fn(usize, usize, &Path)) -> Result<Scan> {
    let files = models_in(input, None)?;
    let mut s = Scan { files: files.len(), ..Default::default() };
    for (i, f) in files.iter().enumerate() {
        progress(i, files.len(), f);
        let rows = match report(f) {
            Ok(r) => r,
            Err(e) => {
                s.errors.push(format!("{e:#}"));
                continue;
            }
        };
        let (mut n, mut b) = (0, 0);
        for r in &rows {
            s.textures += 1;
            s.embedded_bytes += r.bytes;
            s.wrong_stride += r.wrong_stride.is_some() as usize;
            let side = r.width.max(r.height);
            let over = opts.base.max_size.is_none_or(|m| side > m);
            if picks(&r.name, r.format, opts) && over {
                n += 1;
                b += r.bytes;
                let mut after = r.bytes;
                if let Some(m) = opts.base.max_size {
                    let mut sd = side;
                    while sd > m {
                        sd /= 2;
                        after /= 4;
                    }
                }
                s.to_shrink_after += after;
            }
        }
        if n > 0 {
            s.to_shrink += n;
            s.to_shrink_bytes += b;
            s.per_file.push((f.clone(), n, b));
        }
    }
    Ok(s)
}

// ---------------------------------------------------------------- one texture (the viewer)

/// Shrinks the embedded texture(s) called `name` so the longer side is at most `max_side`, keeping the format and a full
/// mip chain, and writes the model to `out` (which may be `input`: the file is read whole first). Returns a one-line
/// description.
pub fn shrink_embedded(input: &Path, name: &str, max_side: u32, out: &Path) -> Result<String> {
    let (file, m) = read_model(input)?;
    let opts = YddOptimizeOptions { base: OptimizeOptions { max_size: Some(max_side), ..Default::default() }, exact_names: vec![name.to_string()], ..Default::default() };
    let (bytes, r) = optimize_bytes(input, &file, m, &opts)?;
    let Some((_, _, c)) = r.textures.iter().find(|t| t.2.changed()) else {
        bail!("'{name}' is already {max_side} px or smaller, or cannot be re-encoded: nothing changed");
    };
    write_file(out, &bytes)?;
    let (b, a) = (&c.before, &c.after);
    Ok(format!("{name}: {}x{} {} -> {}x{} {} ({} -> {} bytes); model {} -> {} bytes", b.width, b.height, b.format, a.width, a.height, a.format, b.bytes, a.bytes, r.file_before, r.file_after))
}

/// Replaces the embedded texture(s) called `name` with `src` (a `.dds` as it is, or an image encoded per `opts`) and
/// writes the model to `out` (which may be `input`). Returns a one-line description.
pub fn replace_embedded(input: &Path, name: &str, src: &Path, out: &Path, opts: ReplaceOpts) -> Result<String> {
    let (file, mut m) = read_model(input)?;
    let list = m.textures();
    let mut textures: Vec<YtdTexture> = list.iter().map(|e| texture_of(&m.g, e.texture)).collect();
    let hits: Vec<usize> = (0..textures.len()).filter(|&i| textures[i].name.eq_ignore_ascii_case(name)).collect();
    let Some(&first) = hits.first() else { bail!("no embedded texture named '{name}' in {}", input.display()) };
    let old = textures[first].clone();
    let new = replacement_for(&old, src, opts)?;
    for &i in &hits {
        replace_texture(&mut m.g, list[i].texture, &new)?;
        textures[i] = new.clone();
    }
    let kind = m.kind;
    let bytes = m.write()?;
    verify(kind, &file, &bytes, &textures).with_context(|| format!("{}: the rebuilt file did not check out, nothing was written", input.display()))?;
    write_file(out, &bytes)?;
    Ok(format!("{name}: {}x{} {} -> {}x{} {}; model {} -> {} bytes", old.width, old.height, old.format, new.width, new.height, new.format, file.len(), bytes.len()))
}

/// Saves the embedded texture called `name` as `.dds` (the stored pixels) or as an image (`.png`, `.jpg`, ...).
pub fn export_embedded(input: &Path, name: &str, out: &Path) -> Result<()> {
    let (_, m) = read_model(input)?;
    let t = m.textures().iter().map(|e| texture_of(&m.g, e.texture)).find(|t| t.name.eq_ignore_ascii_case(name)).with_context(|| format!("no embedded texture named '{name}' in {}", input.display()))?;
    if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds")) {
        fs::write(out, t.to_dds()).with_context(|| format!("writing {}", out.display()))?;
    } else {
        rage_formats::texture_utils::to_rgba_image(&t)?.save(out).with_context(|| format!("writing {}", out.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- verification

/// The rebuilt file reads back with both readers; everything but the textures equals the original; the textures
/// are `expected`, in order (the original's embedded textures for a rebuild that changes none).
pub fn verify(kind: ModelKind, original: &[u8], rebuilt: &[u8], expected: &[YtdTexture]) -> Result<()> {
    let m = Model::read(rebuilt, kind).context("the block reader cannot read it back")?;
    let got: Vec<YtdTexture> = m.textures().iter().map(|e| texture_of(&m.g, e.texture)).collect();
    if got.len() != expected.len() {
        bail!("{} embedded textures read back, {} expected", got.len(), expected.len());
    }
    for (a, b) in got.iter().zip(expected) {
        if (a.name.as_str(), a.width, a.height, a.format, a.levels, a.stride) != (b.name.as_str(), b.width, b.height, b.format, b.levels, b.stride) || a.pixel_data != b.pixel_data {
            bail!("texture '{}' reads back as {}x{} {} {}mip, expected {}x{} {} {}mip", b.name, a.width, a.height, a.format, a.levels, b.width, b.height, b.format, b.levels);
        }
    }
    let dk = match kind {
        ModelKind::Ydd => DrawableKind::Ydd,
        ModelKind::Ydr => DrawableKind::Ydr,
    };
    let before = parse_drawables(original, dk).context("the drawable parser cannot read the original")?;
    let after = parse_drawables(rebuilt, dk).context("the drawable parser cannot read the rebuilt file")?;
    if before.len() != after.len() {
        bail!("{} drawables before, {} after", before.len(), after.len());
    }
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        let (fa, fb) = (fingerprint(a), fingerprint(b));
        if fa != fb {
            let at = fa.lines().zip(fb.lines()).position(|(x, y)| x != y).unwrap_or(0);
            bail!("drawable {i} differs after the rebuild: '{}' became '{}'", fa.lines().nth(at).unwrap_or(""), fb.lines().nth(at).unwrap_or(""));
        }
        let names = |e: &DrawableEntry| e.drawable.shader_group.as_ref().map(|s| s.textures.iter().map(|t| t.name.clone()).collect::<Vec<_>>());
        if names(a) != names(b) {
            bail!("drawable {i}: the embedded texture names changed");
        }
    }
    Ok(())
}

fn digest<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// What a drawable is, without file positions and without texture pixels.
fn fingerprint(e: &DrawableEntry) -> String {
    let d = &e.drawable;
    let mut s = String::new();
    let _ = writeln!(s, "entry {:08X} {}", e.hash, e.name);
    let _ = writeln!(s, "name {} {:08X}", d.name, d.name_hash);
    let _ = writeln!(s, "bounds {:?}", d.bounds);
    let _ = writeln!(s, "lod distances {:?} masks {:?}", d.lod_distances, d.render_masks);
    let _ = writeln!(s, "skeleton {:?}", d.skeleton);
    if let Some(sg) = &d.shader_group {
        for (i, fx) in sg.shaders.iter().enumerate() {
            let _ = writeln!(s, "shader {i} {:08X} {:08X} bucket {} mask {:08X} params {} tex {}", fx.name_hash, fx.file_name_hash, fx.render_bucket, fx.render_bucket_mask, fx.parameter_count, fx.texture_parameter_count);
            for p in &fx.parameters {
                let v = match &p.value {
                    ShaderParameterValue::Texture { name, name_hash } => format!("texture {name} {name_hash:08X}"),
                    ShaderParameterValue::Vectors(v) => format!("{v:?}"),
                    ShaderParameterValue::Pointer => "pointer".into(),
                };
                let _ = writeln!(s, "  param {:08X} type {} {v}", p.name_hash, p.data_type);
            }
        }
    }
    for lod in &d.lods {
        let _ = writeln!(s, "lod {}", lod.level);
        for (m, model) in lod.models.iter().enumerate() {
            let _ = writeln!(s, " model {m} binding {:08X} mask {:04X} mapping {:?}", model.skeleton_binding, model.render_mask_flags, model.shader_mapping);
            for (gi, geo) in model.geometries.iter().enumerate() {
                let vb = geo.vertex_buffer.as_ref().map(|v| {
                    let decl = v.declaration.as_ref().map(|d| (d.flags, d.stride, d.count, d.types));
                    format!("stride {} count {} decl {decl:?} data {:016X}", v.vertex_stride, v.vertex_count, digest(&v.data))
                });
                let ib = geo.index_buffer.as_ref().map(|i| format!("{} {:016X}", i.indices_count, digest(&i.indices)));
                // the triangle count is left out: it is written as indices / 3 (as the reference tool writes it), and
                // some tools leave it 0
                let _ = writeln!(s, "  geometry {gi} shader {} idx {} vtx {} stride {} bones {:?} vb {vb:?} ib {ib:?}", geo.shader_id, geo.indices_count, geo.vertices_count, geo.vertex_stride, geo.bone_ids);
            }
        }
    }
    s
}
