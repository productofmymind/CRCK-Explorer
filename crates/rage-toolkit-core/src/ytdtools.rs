//! YTD editing: merge, split, rename, remove, optimize, and a report that flags likely problems.
//! Gen9 (Enhanced) inputs are converted to Legacy on read (`pipeline::read_ytd`); everything written is a Legacy YTD.
//! A texture `rage-formats` cannot parse is skipped by its reader (it prints a warning), so it is missing from the output too.
use crate::pipeline::{self, Options, Pow2, TextureReport};
use anyhow::{bail, Context, Result};
use image::imageops::{resize, FilterType};
use rage_formats::{
    rage_joaat,
    texture_encode::{encode_texture, is_normal_map_name, EncodeFormat},
    texture_utils::decompress_texture,
    ytd::{full_mip_count, mip_chain_size, serialize_ytd, ytd_chain_size, TextureFormat, YtdTexture},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

const MIB: u64 = 1024 * 1024;
/// Default `split` size: one byte under the 12 MB at which `validate --fivem` starts to warn (it errors above ~16 MB).
/// Both figures are community-documented, not measured by us (see `fivem.rs`).
pub const DEFAULT_SPLIT_BYTES: u64 = 12 * MIB - 1;
/// `optimize --target` never shrinks a texture whose longer side is already below 2x this
const MIN_SHRINK_SIDE: u32 = 32;

fn lc(s: &str) -> String {
    s.to_lowercase()
}

/// the hash a dictionary is keyed by (what `serialize_ytd` writes)
fn hash_of(t: &YtdTexture) -> u32 {
    if t.name_hash != 0 {
        t.name_hash
    } else {
        rage_joaat(&lc(&t.name))
    }
}

fn load(path: &Path) -> Result<Vec<YtdTexture>> {
    let (textures, _) = pipeline::read_ytd(path)?;
    if let Some(t) = textures.iter().find(|t| t.name.is_empty()) {
        bail!("{}: a texture has no readable name (name hash 0x{:08X}); not supported", path.display(), t.name_hash);
    }
    Ok(textures)
}

fn write_ytd(out: &Path, textures: &[YtdTexture]) -> Result<u64> {
    let bytes = serialize_ytd(textures)?;
    fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    Ok(bytes.len() as u64)
}

/// names compare case-insensitively, and a dictionary is looked up by name hash, so both must be unique
fn check_unique(textures: &[YtdTexture]) -> Result<()> {
    let mut names: HashMap<String, &str> = HashMap::new();
    let mut hashes: HashMap<u32, &str> = HashMap::new();
    for t in textures {
        if let Some(prev) = names.insert(lc(&t.name), &t.name) {
            bail!("two textures would be called '{}' and '{}' (names compare case-insensitively)", prev, t.name);
        }
        if let Some(prev) = hashes.insert(hash_of(t), &t.name) {
            bail!("name-hash collision: '{}' and '{}' both hash to 0x{:08X}; a dictionary looks textures up by hash, so one of them must be renamed", prev, t.name, hash_of(t));
        }
    }
    Ok(())
}

/// indices of the named textures (case-insensitive); every unknown name is reported at once
fn resolve(textures: &[YtdTexture], names: &[&str]) -> Result<Vec<usize>> {
    let by_name: HashMap<String, usize> = textures.iter().enumerate().map(|(i, t)| (lc(&t.name), i)).collect();
    let unknown: Vec<String> = names.iter().filter(|n| !by_name.contains_key(&lc(n))).map(|n| format!("'{n}'")).collect();
    if !unknown.is_empty() {
        bail!("no texture named {} in the YTD (it has: {})", unknown.join(", "), textures.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "));
    }
    Ok(names.iter().map(|n| by_name[&lc(n)]).collect())
}

fn row_of(t: &YtdTexture, note: Option<String>) -> TextureReport {
    TextureReport { name: t.name.clone(), width: t.width as u32, height: t.height as u32, format: t.format.to_string(), levels: t.levels, bytes: t.pixel_data.len(), note }
}

fn pow2(n: u32) -> bool {
    n != 0 && n & (n - 1) == 0
}

pub fn parse_pow2(s: &str) -> Result<Pow2> {
    Ok(match s {
        "round" => Pow2::Round,
        "down" => Pow2::Down,
        "up" => Pow2::Up,
        "keep" => Pow2::Keep,
        o => bail!("pow2 must be round|down|up|keep, got '{o}'"),
    })
}

/// run `f` over `items` on all cores, results in input order
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(items.len());
    std::thread::scope(|sc| {
        for _ in 0..workers {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

// ---------------------------------------------------------------- merge

/// What to do when two inputs hold a texture with the same name (case-insensitive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDuplicate {
    Error,
    KeepFirst,
    KeepLast,
}

/// One duplicate that was resolved: the copy from `kept` stays, the one from `dropped` is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateDecision {
    pub name: String,
    pub kept: PathBuf,
    pub dropped: PathBuf,
}

#[derive(Debug)]
pub struct Merged {
    pub textures: usize,
    /// size of the written file
    pub bytes: u64,
    pub decisions: Vec<DuplicateDecision>,
}

/// Combine YTDs (Legacy or Gen9) into one Legacy YTD. Different names whose hashes collide are always an error.
pub fn merge(inputs: &[&Path], out: &Path, on_duplicate: OnDuplicate) -> Result<Merged> {
    if inputs.is_empty() {
        bail!("nothing to merge: give at least one input YTD");
    }
    // first-seen order; the input a texture came from rides along
    let mut kept: Vec<(usize, YtdTexture)> = vec![];
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut decisions = vec![];
    for (i, path) in inputs.iter().enumerate() {
        for t in load(path)? {
            let Some(&k) = at.get(&lc(&t.name)) else {
                at.insert(lc(&t.name), kept.len());
                kept.push((i, t));
                continue;
            };
            let first = kept[k].0;
            match on_duplicate {
                OnDuplicate::Error => bail!(
                    "texture '{}' is in {} and in {}; pass --on-duplicate first|last to pick one",
                    t.name,
                    inputs[first].display(),
                    path.display()
                ),
                OnDuplicate::KeepFirst => decisions.push(DuplicateDecision { name: kept[k].1.name.clone(), kept: inputs[first].to_path_buf(), dropped: path.to_path_buf() }),
                OnDuplicate::KeepLast => {
                    decisions.push(DuplicateDecision { name: t.name.clone(), kept: path.to_path_buf(), dropped: inputs[first].to_path_buf() });
                    kept[k] = (i, t);
                }
            }
        }
    }
    let textures: Vec<YtdTexture> = kept.into_iter().map(|(_, t)| t).collect();
    check_unique(&textures)?;
    let bytes = write_ytd(out, &textures)?;
    Ok(Merged { textures: textures.len(), bytes, decisions })
}

// ---------------------------------------------------------------- split

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitBy {
    /// pack textures into as few files as possible, each at most this many bytes on disk
    MaxSize(u64),
    PerTexture,
    /// group by the first n characters of the (lowercased) name
    Prefix(usize),
}

#[derive(Debug, Clone)]
pub struct SplitPart {
    pub path: PathBuf,
    pub textures: Vec<String>,
    /// size of the written file
    pub bytes: u64,
    /// `MaxSize` only: this file is bigger than asked (a single texture that does not fit)
    pub over_limit: bool,
}

/// first-fit decreasing on each texture's size as a file of its own
fn first_fit(mut textures: Vec<(u64, YtdTexture)>, budget: u64) -> Vec<Vec<YtdTexture>> {
    textures.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    let mut groups: Vec<(u64, Vec<YtdTexture>)> = vec![];
    for (n, t) in textures {
        match groups.iter_mut().find(|g| g.0 + n <= budget) {
            Some(g) => {
                g.0 += n;
                g.1.push(t);
            }
            None => groups.push((n, vec![t])),
        }
    }
    groups.into_iter().map(|g| g.1).collect()
}

/// serialize `group`; when the real file is over `max` (the per-texture sizes added up optimistically), cut it in two and retry
fn fit(group: Vec<YtdTexture>, max: u64, done: &mut Vec<(Vec<YtdTexture>, Vec<u8>)>) -> Result<()> {
    let bytes = serialize_ytd(&group)?;
    if bytes.len() as u64 <= max || group.len() == 1 {
        done.push((group, bytes));
        return Ok(());
    }
    let total: usize = group.iter().map(|t| t.pixel_data.len()).sum();
    let mut acc = 0;
    let mut cut = group.len() - 1;
    for (i, t) in group.iter().enumerate() {
        acc += t.pixel_data.len();
        if acc * 2 >= total {
            cut = (i + 1).min(group.len() - 1);
            break;
        }
    }
    let mut a = group;
    let b = a.split_off(cut);
    fit(a, max, done)?;
    fit(b, max, done)
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || "_-+".contains(c) { c } else { '_' }).collect()
}

/// Write the textures of `input` as several YTDs named `<input stem>_<suffix>.ytd` in `out_dir` (suffix: a running
/// number, the texture name, or the prefix).
pub fn split(input: &Path, out_dir: &Path, by: SplitBy) -> Result<Vec<SplitPart>> {
    let textures = load(input)?;
    if textures.is_empty() {
        bail!("{} has no textures", input.display());
    }
    let stem = input.file_stem().map_or_else(|| "ytd".to_string(), |s| s.to_string_lossy().into_owned());
    let mut parts: Vec<(String, Vec<YtdTexture>, Vec<u8>)> = vec![];
    let max = match by {
        SplitBy::MaxSize(max) => {
            if max == 0 {
                bail!("the size limit must be above zero");
            }
            // files are deflated, so weigh each texture by its own file size; a few percent of slack for what
            // does not add up, and `fit` checks the real size of every group
            let weights = par_map(&textures, |t| serialize_ytd(std::slice::from_ref(t)).map(|b| b.len() as u64)).into_iter().collect::<Result<Vec<_>>>()?;
            let mut groups = vec![];
            for g in first_fit(weights.into_iter().zip(textures).collect(), max - max / 32) {
                fit(g, max, &mut groups)?;
            }
            let width = groups.len().to_string().len();
            parts.extend(groups.into_iter().enumerate().map(|(i, (g, b))| (format!("{:0width$}", i + 1), g, b)));
            Some(max)
        }
        SplitBy::PerTexture => {
            for t in textures {
                let b = serialize_ytd(std::slice::from_ref(&t))?;
                parts.push((t.name.clone(), vec![t], b));
            }
            None
        }
        SplitBy::Prefix(n) => {
            if n == 0 {
                bail!("the prefix length must be at least 1");
            }
            let mut groups: BTreeMap<String, Vec<YtdTexture>> = BTreeMap::new();
            for t in textures {
                groups.entry(lc(&t.name).chars().take(n).collect()).or_default().push(t);
            }
            for (p, g) in groups {
                let b = serialize_ytd(&g)?;
                parts.push((p, g, b));
            }
            None
        }
    };
    let paths: Vec<PathBuf> = parts.iter().map(|p| out_dir.join(format!("{stem}_{}.ytd", sanitize(&p.0)))).collect();
    let mut seen = HashSet::new();
    for p in &paths {
        if !seen.insert(lc(&p.to_string_lossy())) {
            bail!("two parts would both be written to {}", p.display());
        }
    }
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut done = vec![];
    for (path, (_, group, bytes)) in paths.into_iter().zip(parts) {
        fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
        let mut names: Vec<String> = group.iter().map(|t| t.name.clone()).collect();
        names.sort_by_key(|n| lc(n));
        done.push(SplitPart { path, textures: names, over_limit: max.is_some_and(|m| bytes.len() as u64 > m), bytes: bytes.len() as u64 });
    }
    Ok(done)
}

// ---------------------------------------------------------------- rename / remove

/// Rename textures (`old`, `new`) at once, so a swap works. The name hash is recomputed; pixels are untouched.
/// Returns the renames done, with the old name as the YTD spells it.
pub fn rename(input: &Path, out: &Path, map: &[(&str, &str)]) -> Result<Vec<(String, String)>> {
    if map.is_empty() {
        bail!("nothing to rename");
    }
    let mut textures = load(input)?;
    let idx = resolve(&textures, &map.iter().map(|m| m.0).collect::<Vec<_>>())?;
    let mut seen = HashSet::new();
    for (&i, (_, new)) in idx.iter().zip(map) {
        if !seen.insert(i) {
            bail!("'{}' is renamed twice", textures[i].name);
        }
        if new.is_empty() || new.chars().any(char::is_control) {
            bail!("'{new}' is not a usable texture name");
        }
    }
    let mut done = vec![];
    for (&i, (_, new)) in idx.iter().zip(map) {
        done.push((textures[i].name.clone(), new.to_string()));
        textures[i].name = new.to_string();
        textures[i].name_hash = rage_joaat(&lc(new));
    }
    check_unique(&textures)?;
    write_ytd(out, &textures)?;
    Ok(done)
}

/// Drop the named textures. Returns their names as the YTD spells them. Removing every texture is refused.
pub fn remove(input: &Path, out: &Path, names: &[&str]) -> Result<Vec<String>> {
    if names.is_empty() {
        bail!("nothing to remove");
    }
    let textures = load(input)?;
    let gone: HashSet<usize> = resolve(&textures, names)?.into_iter().collect();
    if gone.len() == textures.len() {
        bail!("that removes every texture; a YTD needs at least one");
    }
    let (out_tex, kept): (Vec<_>, Vec<_>) = textures.into_iter().enumerate().partition(|(i, _)| gone.contains(i));
    let kept: Vec<YtdTexture> = kept.into_iter().map(|(_, t)| t).collect();
    write_ytd(out, &kept)?;
    Ok(out_tex.into_iter().map(|(_, t)| t.name).collect())
}

// ---------------------------------------------------------------- optimize

#[derive(Clone, Debug)]
pub struct OptimizeOptions {
    /// textures with a longer side are resized down to this (Lanczos); nothing is ever enlarged
    pub max_size: Option<u32>,
    /// how the resized sides are snapped (as in `pipeline::Options`)
    pub pow2: Pow2,
    /// drop mip levels whose longer side is below this many pixels (level 0 always stays). Saves very little.
    pub drop_mips_below: Option<u32>,
    /// re-encode every texture that is not in this format yet
    pub format_override: Option<EncodeFormat>,
    /// after the above, halve the largest textures until the pixel data totals at most this many bytes
    pub target_bytes: Option<u64>,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self { max_size: None, pow2: Pow2::Round, drop_mips_below: None, format_override: None, target_bytes: None }
    }
}

#[derive(Debug, Clone)]
pub struct TextureChange {
    pub before: TextureReport,
    /// `after.note` says what was done
    pub after: TextureReport,
}

impl TextureChange {
    pub fn changed(&self) -> bool {
        (self.before.width, self.before.height, &self.before.format, self.before.levels, self.before.bytes) != (self.after.width, self.after.height, &self.after.format, self.after.levels, self.after.bytes)
    }
}

#[derive(Debug)]
pub struct Optimized {
    pub textures: Vec<TextureChange>,
    /// pixel data totals
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// file sizes
    pub file_before: u64,
    pub file_after: u64,
    /// `Some` when a target was asked for: whether the pixel data fits it
    pub target_met: Option<bool>,
}

fn encode_format_of(f: TextureFormat) -> Option<EncodeFormat> {
    Some(match f {
        TextureFormat::DXT1 => EncodeFormat::Bc1,
        TextureFormat::DXT3 | TextureFormat::DXT5 => EncodeFormat::Bc3,
        TextureFormat::ATI1 => EncodeFormat::Bc4,
        TextureFormat::ATI2 => EncodeFormat::Bc5,
        TextureFormat::BC7 => EncodeFormat::Bc7,
        TextureFormat::A8R8G8B8 => EncodeFormat::Rgba8,
        _ => return None,
    })
}

/// Size under the cap: the pipeline's pow2 policy, but never larger than the source. A side the policy would enlarge
/// (a non-power-of-two source, e.g. 500 -> 512) goes to the power of two below instead; with `Pow2::Keep` it stays a
/// multiple of 4.
fn capped(w: u32, h: u32, cap: u32, pow2: Pow2) -> (u32, u32) {
    let (tw, th) = pipeline::target_size(w, h, &Options { max_size: Some(cap), pow2, format: None });
    let side = |t: u32, src: u32| {
        if t > src && pow2 != Pow2::Keep && src >= 4 {
            1 << (31 - src.leading_zeros())
        } else {
            (t.min(src) & !3).max(4).min(src)
        }
    };
    (side(tw, w), side(th, h))
}

/// The encoder in rage-formats lays out a block-compressed mip chain only while every level is a multiple of 4 on both
/// sides, so a non-power-of-two texture (500 high: 500, 250 ...) cannot get a full chain. Other formats: no limit.
fn encodable_levels(format: TextureFormat, w: u32, h: u32) -> u8 {
    if !format.is_block_compressed() {
        return u8::MAX;
    }
    (0..32u32).take_while(|&i| (w >> i) >= 4 && (h >> i) >= 4 && (w >> i) % 4 == 0 && (h >> i) % 4 == 0).count().max(1) as u8
}

fn halve(d: u32) -> u32 {
    ((d / 2) & !3).max(4)
}

/// bytes of a mip chain as the game stores it
fn chain_bytes(stride: u16, height: u16, levels: u8) -> usize {
    let mut level = stride as usize * height as usize;
    let mut total = 0;
    for _ in 0..levels {
        total += level;
        level /= 4;
    }
    total
}

struct Plan {
    w: u32,
    h: u32,
    format: TextureFormat,
    levels: u8,
    reencode: bool,
    /// resizing was wanted but this format has no encoder
    skipped: bool,
}

fn drop_levels(levels: u8, w: u32, h: u32, opts: &OptimizeOptions) -> u8 {
    let Some(n) = opts.drop_mips_below else { return levels };
    let keep = (0..levels as u32).take_while(|&i| w.checked_shr(i).unwrap_or(0).max(h.checked_shr(i).unwrap_or(0)) >= n).count().max(1);
    levels.min(keep as u8)
}

fn plan_for(t: &YtdTexture, opts: &OptimizeOptions, size: (u32, u32)) -> Plan {
    let (w0, h0) = (t.width as u32, t.height as u32);
    let resized = size != (w0, h0);
    let target = match opts.format_override {
        Some(f) => Some(f.texture_format()),
        None if resized => encode_format_of(t.format).map(EncodeFormat::texture_format),
        None => Some(t.format),
    };
    let Some(format) = target else {
        return Plan { w: w0, h: h0, format: t.format, levels: drop_levels(t.levels, w0, h0, opts), reencode: false, skipped: true };
    };
    let reencode = resized || format != t.format;
    let levels = if !reencode {
        t.levels
    } else {
        // a complete chain stays complete; a short one keeps its length
        let full = full_mip_count(format, size.0 as u16, size.1 as u16).min(encodable_levels(format, size.0, size.1));
        if t.levels >= full_mip_count(t.format, t.width, t.height) {
            full
        } else {
            t.levels.min(full)
        }
    };
    Plan { w: size.0, h: size.1, format, levels: drop_levels(levels, size.0, size.1, opts), reencode, skipped: false }
}

fn plan_bytes(t: &YtdTexture, p: &Plan) -> usize {
    if p.reencode {
        ytd_chain_size(p.format, p.w as u16, p.h as u16, p.levels)
    } else if p.levels < t.levels {
        chain_bytes(t.stride, t.height, p.levels)
    } else {
        t.pixel_data.len()
    }
}

/// halve the biggest textures until the total fits, or nothing more can shrink
fn shrink_to_target(src: &[YtdTexture], plans: &mut [Plan], opts: &OptimizeOptions, target: u64) {
    loop {
        let sizes: Vec<usize> = src.iter().zip(plans.iter()).map(|(t, p)| plan_bytes(t, p)).collect();
        if sizes.iter().sum::<usize>() as u64 <= target {
            return;
        }
        let pick = (0..src.len())
            .filter(|&i| plans[i].w.max(plans[i].h) >= 2 * MIN_SHRINK_SIDE && (opts.format_override.is_some() || encode_format_of(src[i].format).is_some()))
            .max_by_key(|&i| sizes[i]);
        let Some(i) = pick else { return };
        plans[i] = plan_for(&src[i], opts, (halve(plans[i].w), halve(plans[i].h)));
    }
}

/// decode the top level, resize, encode again with the planned format and chain length
fn reencode(t: &YtdTexture, p: &Plan) -> Result<(YtdTexture, Option<&'static str>)> {
    let rgba = decompress_texture(t).with_context(|| format!("decoding '{}' ({})", t.name, t.format))?;
    let mut img = image::RgbaImage::from_raw(t.width as u32, t.height as u32, rgba).context("decoded pixels do not match the texture size")?;
    if (p.w, p.h) != img.dimensions() {
        img = resize(&img, p.w, p.h, FilterType::Lanczos3);
    }
    let fmt = encode_format_of(p.format).with_context(|| format!("no encoder for {}", p.format))?;
    let warn = (fmt == EncodeFormat::Bc1 && img.pixels().any(|px| px[3] != 255)).then_some("transparency lost (DXT1 has no smooth alpha)");
    let mut tex = encode_texture(&t.name, img.as_raw(), p.w, p.h, fmt, Some(p.levels)).with_context(|| format!("re-encoding '{}'", t.name))?;
    tex.name_hash = t.name_hash;
    Ok((tex, warn))
}

fn describe(t: &YtdTexture, n: &YtdTexture, p: &Plan, warn: Option<&str>) -> Option<String> {
    let mut parts = vec![];
    if p.skipped {
        parts.push(format!("left as is: {} cannot be re-encoded", t.format));
    }
    if (t.width, t.height) != (n.width, n.height) {
        parts.push(format!("resized {}x{} -> {}x{}", t.width, t.height, n.width, n.height));
    }
    if t.format != n.format {
        parts.push(format!("{} -> {}", t.format, n.format));
    }
    if t.levels != n.levels {
        parts.push(format!("mips {} -> {}", t.levels, n.levels));
    }
    parts.extend(warn.map(str::to_string));
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// Shrink a YTD. A texture that no option touches is copied unchanged (same pixels, same bytes). Names are kept.
/// Order of work: cap the size, re-encode to `format_override`, drop small mips, then (if asked) shrink to `target_bytes`.
pub fn optimize(input: &Path, out: &Path, opts: &OptimizeOptions) -> Result<Optimized> {
    if opts.max_size.is_some_and(|m| m < 4) {
        bail!("the size cap must be at least 4");
    }
    let src = load(input)?;
    let file_before = fs::metadata(input)?.len();
    let mut plans: Vec<Plan> = src
        .iter()
        .map(|t| {
            let (w, h) = (t.width as u32, t.height as u32);
            let size = match opts.max_size {
                Some(cap) if w.max(h) > cap => capped(w, h, cap, opts.pow2),
                _ => (w, h),
            };
            plan_for(t, opts, size)
        })
        .collect();
    if let Some(target) = opts.target_bytes {
        shrink_to_target(&src, &mut plans, opts, target);
    }

    let jobs: Vec<usize> = (0..src.len()).filter(|&i| plans[i].reencode).collect();
    let mut encoded: HashMap<usize, _> = jobs.iter().copied().zip(par_map(&jobs, |&i| reencode(&src[i], &plans[i]))).collect();

    let (mut textures, mut changes) = (vec![], vec![]);
    for (i, t) in src.iter().enumerate() {
        let p = &plans[i];
        let (n, warn) = if p.reencode {
            encoded.remove(&i).unwrap()?
        } else {
            let mut n = t.clone();
            if p.levels < t.levels {
                n.levels = p.levels;
                n.pixel_data.truncate(chain_bytes(t.stride, t.height, p.levels));
            }
            (n, None)
        };
        changes.push(TextureChange { before: row_of(t, None), after: row_of(&n, describe(t, &n, p, warn)) });
        textures.push(n);
    }
    let file_after = write_ytd(out, &textures)?;
    let bytes_before = src.iter().map(|t| t.pixel_data.len()).sum();
    let bytes_after = textures.iter().map(|t| t.pixel_data.len()).sum::<usize>();
    Ok(Optimized { textures: changes, bytes_before, bytes_after, file_before, file_after, target_met: opts.target_bytes.map(|tb| bytes_after as u64 <= tb) })
}

pub fn format_optimize(o: &Optimized) -> String {
    let w = o.textures.iter().map(|c| c.before.name.len()).max().unwrap_or(4).max(4);
    let mut s = String::new();
    for c in &o.textures {
        let (b, a) = (&c.before, &c.after);
        if c.changed() {
            let _ = writeln!(s, "{:<w$}  {}x{} {} {}mip {} B  ->  {}x{} {} {}mip {} B  {}", b.name, b.width, b.height, b.format, b.levels, b.bytes, a.width, a.height, a.format, a.levels, a.bytes, a.note.as_deref().unwrap_or(""));
        } else {
            let _ = writeln!(s, "{:<w$}  unchanged ({}x{} {} {}mip {} B){}", b.name, b.width, b.height, b.format, b.levels, b.bytes, a.note.as_ref().map(|n| format!("  {n}")).unwrap_or_default());
        }
    }
    let _ = writeln!(s, "pixel data {} -> {} bytes; file {} -> {} bytes", o.bytes_before, o.bytes_after, o.file_before, o.file_after);
    s
}

// ---------------------------------------------------------------- report

/// A likely problem. `code` is stable (NON_POW2, NOT_MULTIPLE_OF_4, MISSING_MIPS, ALPHA_UNUSED, NORMAL_MAP_FORMAT,
/// DUPLICATE_CONTENT, DECODE_FAILED); these are hints, none of them is an error by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub levels: u8,
    /// pixel data as stored in the YTD
    pub bytes: usize,
    /// estimate of the GPU memory: the whole mip chain with each level rounded up to whole blocks (no driver padding)
    pub vram: usize,
    pub flags: Vec<Flag>,
}

/// One row per texture (sorted by name) with its flags.
pub fn report(input: &Path) -> Result<Vec<ReportRow>> {
    let textures = load(input)?;
    type Key<'a> = (u16, u16, u32, u8, &'a [u8]);
    fn key(t: &YtdTexture) -> Key<'_> {
        (t.width, t.height, t.format as u32, t.levels, &t.pixel_data[..])
    }
    let mut same: HashMap<Key<'_>, Vec<&str>> = HashMap::new();
    for t in textures.iter().filter(|t| !t.pixel_data.is_empty()) {
        same.entry(key(t)).or_default().push(&t.name);
    }
    let mut rows: Vec<ReportRow> = textures
        .iter()
        .map(|t| {
            let (w, h) = (t.width as u32, t.height as u32);
            let mut flags = vec![];
            let mut flag = |code, detail: String| flags.push(Flag { code, detail });
            if !pow2(w) || !pow2(h) {
                flag("NON_POW2", format!("{w}x{h}: the game wants power-of-two sides"));
            }
            if t.format.is_block_compressed() && (w % 4 != 0 || h % 4 != 0) {
                flag("NOT_MULTIPLE_OF_4", format!("{w}x{h} {}: block formats need sides that are multiples of 4", t.format));
            }
            let full = full_mip_count(t.format, t.width, t.height);
            if t.levels < full {
                flag("MISSING_MIPS", if t.levels <= 1 { format!("no mip chain (1 level, {full} fit)") } else { format!("{} of {full} mip levels: the chain stops early", t.levels) });
            }
            if matches!(t.format, TextureFormat::DXT3 | TextureFormat::DXT5) {
                match decompress_texture(t) {
                    Ok(px) if px.chunks_exact(4).all(|p| p[3] == 255) => flag("ALPHA_UNUSED", format!("every alpha value is 255: {} could be DXT1 at half the size", t.format)),
                    Ok(_) => {}
                    Err(e) => flag("DECODE_FAILED", e.to_string()),
                }
            }
            if is_normal_map_name(&t.name) && !matches!(t.format, TextureFormat::ATI2 | TextureFormat::BC7 | TextureFormat::A8R8G8B8) {
                flag("NORMAL_MAP_FORMAT", format!("named like a normal map but stored as {} (expected ATI2, BC7 or uncompressed)", t.format));
            }
            if let Some(group) = same.get(&key(t)).filter(|g| g.len() > 1) {
                let others: Vec<&str> = group.iter().copied().filter(|n| *n != t.name).collect();
                flag("DUPLICATE_CONTENT", format!("identical pixels to {}", others.join(", ")));
            }
            ReportRow { name: t.name.clone(), width: w, height: h, format: t.format.to_string(), levels: t.levels, bytes: t.pixel_data.len(), vram: mip_chain_size(t.format, t.width, t.height, t.levels), flags }
        })
        .collect();
    rows.sort_by_key(|r| lc(&r.name));
    Ok(rows)
}

pub fn format_report(rows: &[ReportRow]) -> String {
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let mut s = format!("{:<w$}  {:>9}  {:<8} {:>4}  {:>10}  {:>10}  flags\n", "name", "size", "format", "mips", "bytes", "vram~");
    for r in rows {
        let codes = r.flags.iter().map(|f| f.code).collect::<Vec<_>>().join(",");
        let _ = writeln!(s, "{:<w$}  {:>9}  {:<8} {:>4}  {:>10}  {:>10}  {codes}", r.name, format!("{}x{}", r.width, r.height), r.format, r.levels, r.bytes, r.vram);
        for f in &r.flags {
            let _ = writeln!(s, "{:w$}    ! {}: {}", "", f.code, f.detail);
        }
    }
    let _ = writeln!(s, "{} texture(s), {} bytes stored, ~{} bytes VRAM, {} flagged", rows.len(), rows.iter().map(|r| r.bytes).sum::<usize>(), rows.iter().map(|r| r.vram).sum::<usize>(), rows.iter().filter(|r| !r.flags.is_empty()).count());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn tmp(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("rage_toolkit_ytdtools_{}_{}_{tag}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// incompressible pixels
    fn noise(w: u32, h: u32, seed: u32, alpha: u8) -> Vec<u8> {
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(7);
        (0..w * h)
            .flat_map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let b = s.to_le_bytes();
                [b[1], b[2], b[3], alpha]
            })
            .collect()
    }

    /// smooth pixels (resizing them gives a predictable result)
    fn smooth(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, 90, alpha])).collect()
    }

    fn tex(name: &str, w: u32, h: u32, fmt: EncodeFormat, px: Vec<u8>) -> YtdTexture {
        encode_texture(name, &px, w, h, fmt, None).unwrap()
    }

    fn put(dir: &Path, file: &str, ts: &[YtdTexture]) -> PathBuf {
        let p = dir.join(file);
        fs::write(&p, serialize_ytd(ts).unwrap()).unwrap();
        p
    }

    fn read(p: &Path) -> Vec<YtdTexture> {
        let mut t = pipeline::read_ytd(p).unwrap().0;
        t.sort_by_key(|t| lc(&t.name));
        t
    }

    fn names(p: &Path) -> Vec<String> {
        read(p).into_iter().map(|t| t.name).collect()
    }

    fn get(p: &Path, name: &str) -> YtdTexture {
        read(p).into_iter().find(|t| t.name == name).unwrap()
    }

    fn mean_err(a: &[u8], b: &[u8]) -> u64 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() / a.len() as u64
    }

    /// two different names with the same joaat hash
    fn colliding_pair() -> (String, String) {
        let mut seen: HashMap<u32, String> = HashMap::new();
        for n in 0..5_000_000u32 {
            let name = format!("t{n}");
            if let Some(prev) = seen.insert(rage_joaat(&name), name.clone()) {
                return (prev, name);
            }
        }
        panic!("no joaat collision found");
    }

    #[test]
    fn merge_duplicate_policies_and_decisions() {
        let d = tmp("merge");
        let a = put(&d, "a.ytd", &[tex("body_d", 64, 64, EncodeFormat::Bc1, noise(64, 64, 1, 255)), tex("Shared", 32, 32, EncodeFormat::Bc1, noise(32, 32, 2, 255))]);
        let b = put(&d, "b.ytd", &[tex("shared", 32, 32, EncodeFormat::Bc1, noise(32, 32, 3, 255)), tex("extra_d", 16, 16, EncodeFormat::Bc1, noise(16, 16, 4, 255))]);
        let out = d.join("m.ytd");

        let e = merge(&[&a, &b], &out, OnDuplicate::Error).unwrap_err().to_string();
        assert!(e.contains("'shared'") && e.contains("a.ytd") && e.contains("b.ytd"), "{e}");
        assert!(!out.exists(), "an error must not leave an output");

        let m = merge(&[&a, &b], &out, OnDuplicate::KeepFirst).unwrap();
        assert_eq!((m.textures, m.decisions.len()), (3, 1));
        assert_eq!(m.decisions[0], DuplicateDecision { name: "Shared".into(), kept: a.clone(), dropped: b.clone() });
        assert_eq!(names(&out), ["body_d", "extra_d", "Shared"]);
        assert_eq!(get(&out, "Shared").pixel_data, get(&a, "Shared").pixel_data);
        assert_eq!(get(&out, "body_d").pixel_data, get(&a, "body_d").pixel_data);

        let m = merge(&[&a, &b], &out, OnDuplicate::KeepLast).unwrap();
        assert_eq!(m.decisions[0], DuplicateDecision { name: "shared".into(), kept: b.clone(), dropped: a.clone() });
        assert_eq!(names(&out), ["body_d", "extra_d", "shared"]);
        assert_eq!(get(&out, "shared").pixel_data, get(&b, "shared").pixel_data);

        // three inputs: one decision per dropped copy; merging into one of the inputs is fine
        let c = put(&d, "c.ytd", &[tex("SHARED", 32, 32, EncodeFormat::Bc1, noise(32, 32, 5, 255))]);
        let m = merge(&[&a, &b, &c], &a, OnDuplicate::KeepFirst).unwrap();
        assert_eq!(m.decisions.len(), 2);
        assert_eq!(names(&a), ["body_d", "extra_d", "Shared"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn merge_rejects_hash_collisions() {
        let (x, y) = colliding_pair();
        assert_ne!(x, y);
        let d = tmp("collide");
        let a = put(&d, "a.ytd", &[tex(&x, 16, 16, EncodeFormat::Bc1, noise(16, 16, 1, 255))]);
        let b = put(&d, "b.ytd", &[tex(&y, 16, 16, EncodeFormat::Bc1, noise(16, 16, 2, 255))]);
        for policy in [OnDuplicate::Error, OnDuplicate::KeepFirst, OnDuplicate::KeepLast] {
            let e = merge(&[&a, &b], &d.join("m.ytd"), policy).unwrap_err().to_string();
            assert!(e.contains("name-hash collision") && e.contains(&x) && e.contains(&y), "{e}");
        }
        // rename into a collision is refused too
        let c = put(&d, "c.ytd", &[tex(&x, 16, 16, EncodeFormat::Bc1, noise(16, 16, 1, 255)), tex("other", 16, 16, EncodeFormat::Bc1, noise(16, 16, 3, 255))]);
        let e = rename(&c, &d.join("r.ytd"), &[("other", &y)]).unwrap_err().to_string();
        assert!(e.contains("name-hash collision"), "{e}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn split_by_size_packs_and_keeps_every_texture() {
        let d = tmp("split");
        // 8 incompressible 128x128 BC3 textures, about 22 KB each with mips
        let all: Vec<YtdTexture> = (0..8).map(|i| tex(&format!("t{i}_d"), 128, 128, EncodeFormat::Bc3, noise(128, 128, i + 10, 255))).collect();
        let one = serialize_ytd(&all[..1]).unwrap().len() as u64;
        let src = put(&d, "veh.ytd", &all);
        let max = one * 3 + one / 2; // 3 per file
        let parts = split(&src, &d.join("out"), SplitBy::MaxSize(max)).unwrap();
        assert_eq!(parts.len(), 3, "{parts:?}");
        let mut seen = vec![];
        for p in &parts {
            assert!(p.bytes <= max && !p.over_limit);
            assert_eq!(fs::metadata(&p.path).unwrap().len(), p.bytes);
            assert!(p.path.file_name().unwrap().to_string_lossy().starts_with("veh_"));
            for t in read(&p.path) {
                assert_eq!(t.pixel_data, get(&src, &t.name).pixel_data, "{} changed", t.name);
                seen.push(t.name);
            }
        }
        seen.sort();
        assert_eq!(seen, names(&src));

        // everything fits: one file
        let parts = split(&src, &d.join("one"), SplitBy::MaxSize(DEFAULT_SPLIT_BYTES)).unwrap();
        assert_eq!(parts.len(), 1);
        // a single texture over the limit is kept whole and flagged
        let parts = split(&src, &d.join("tiny"), SplitBy::MaxSize(one / 2)).unwrap();
        assert_eq!(parts.len(), 8);
        assert!(parts.iter().all(|p| p.over_limit && p.textures.len() == 1));
        assert!(split(&src, &d.join("zero"), SplitBy::MaxSize(0)).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn split_per_texture_and_by_prefix() {
        let d = tmp("split2");
        let src = put(
            &d,
            "pack.ytd",
            &[
                tex("lspd_d", 16, 16, EncodeFormat::Bc1, noise(16, 16, 1, 255)),
                tex("lspd_n", 16, 16, EncodeFormat::Bc5, noise(16, 16, 2, 255)),
                tex("Body_d", 16, 16, EncodeFormat::Bc1, noise(16, 16, 3, 255)),
                tex("b", 16, 16, EncodeFormat::Bc1, noise(16, 16, 4, 255)),
            ],
        );
        let parts = split(&src, &d.join("each"), SplitBy::PerTexture).unwrap();
        assert_eq!(parts.len(), 4);
        assert!(d.join("each").join("pack_lspd_n.ytd").exists());
        assert_eq!(names(&d.join("each").join("pack_Body_d.ytd")), ["Body_d"]);

        let parts = split(&src, &d.join("pre"), SplitBy::Prefix(2)).unwrap();
        let by: Vec<(String, Vec<String>)> = parts.iter().map(|p| (p.path.file_name().unwrap().to_string_lossy().into_owned(), p.textures.clone())).collect();
        assert_eq!(
            by,
            [
                ("pack_b.ytd".to_string(), vec!["b".to_string()]),
                ("pack_bo.ytd".to_string(), vec!["Body_d".to_string()]),
                ("pack_ls.ytd".to_string(), vec!["lspd_d".to_string(), "lspd_n".to_string()]),
            ]
        );
        assert!(split(&src, &d.join("p0"), SplitBy::Prefix(0)).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rename_is_case_insensitive_swaps_and_rejects_bad_input() {
        let d = tmp("rename");
        let src = put(&d, "r.ytd", &[tex("Alpha_d", 16, 16, EncodeFormat::Bc1, noise(16, 16, 1, 255)), tex("beta_d", 32, 32, EncodeFormat::Bc1, noise(32, 32, 2, 255)), tex("gamma_d", 16, 16, EncodeFormat::Bc1, noise(16, 16, 3, 255))]);
        let out = d.join("o.ytd");
        let done = rename(&src, &out, &[("ALPHA_D", "beta_d"), ("beta_d", "Alpha_d")]).unwrap();
        assert_eq!(done, [("Alpha_d".to_string(), "beta_d".to_string()), ("beta_d".to_string(), "Alpha_d".to_string())]);
        // pixels followed the swap, hashes follow the new names
        assert_eq!(get(&out, "beta_d").pixel_data, get(&src, "Alpha_d").pixel_data);
        assert_eq!(get(&out, "Alpha_d").pixel_data, get(&src, "beta_d").pixel_data);
        for t in read(&out) {
            assert_eq!(t.name_hash, rage_joaat(&t.name.to_lowercase()));
        }

        let e = rename(&src, &out, &[("nope", "x"), ("gamma_d", "y"), ("also_missing", "z")]).unwrap_err().to_string();
        assert!(e.contains("'nope'") && e.contains("'also_missing'") && !e.contains("'gamma_d'"), "{e}");
        let e = rename(&src, &out, &[("gamma_d", "BETA_D")]).unwrap_err().to_string();
        assert!(e.contains("would be called"), "{e}");
        assert!(rename(&src, &out, &[("gamma_d", "x"), ("GAMMA_D", "y")]).is_err());
        assert!(rename(&src, &out, &[("gamma_d", "")]).is_err());
        assert!(rename(&src, &out, &[]).is_err());
        // a change of case alone is fine
        rename(&src, &out, &[("gamma_d", "GAMMA_D")]).unwrap();
        assert!(names(&out).contains(&"GAMMA_D".to_string()));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn remove_by_name() {
        let d = tmp("remove");
        let src = put(&d, "r.ytd", &[tex("a", 16, 16, EncodeFormat::Bc1, noise(16, 16, 1, 255)), tex("B", 16, 16, EncodeFormat::Bc1, noise(16, 16, 2, 255)), tex("c", 16, 16, EncodeFormat::Bc1, noise(16, 16, 3, 255))]);
        let out = d.join("o.ytd");
        let mut gone = remove(&src, &out, &["b", "A", "b"]).unwrap();
        gone.sort();
        assert_eq!(gone, ["B", "a"]);
        assert_eq!(names(&out), ["c"]);
        assert_eq!(get(&out, "c").pixel_data, get(&src, "c").pixel_data);
        let e = remove(&src, &out, &["a", "zzz"]).unwrap_err().to_string();
        assert!(e.contains("'zzz'") && !e.contains("'a'"), "{e}");
        let e = remove(&src, &out, &["a", "b", "c"]).unwrap_err().to_string();
        assert!(e.contains("every texture"), "{e}");
        assert!(remove(&src, &out, &[]).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn optimize_caps_size_without_touching_small_textures() {
        let d = tmp("opt");
        // built the way a user would: a folder of images through the pipeline
        let imgs = d.join("imgs");
        fs::create_dir_all(&imgs).unwrap();
        let png = |name: &str, w: u32, h: u32, a: u8| image::RgbaImage::from_raw(w, h, smooth(w, h, a)).unwrap().save(imgs.join(name)).unwrap();
        png("body_d.png", 512, 512, 255);
        png("decal.png", 64, 64, 128);
        png("body_n.png", 128, 128, 255);
        let src = d.join("src.ytd");
        pipeline::folder_to_ytd(&imgs, &src, &Options::default()).unwrap();

        let out = d.join("out.ytd");
        let r = optimize(&src, &out, &OptimizeOptions { max_size: Some(256), ..Default::default() }).unwrap();
        let by = |n: &str| r.textures.iter().find(|c| c.before.name == n).unwrap();
        assert!(by("body_d").changed());
        assert_eq!((by("body_d").after.width, by("body_d").after.height), (256, 256));
        assert_eq!(by("body_d").after.format, "DXT1");
        assert_eq!(by("body_d").after.levels, 7); // a full chain stays a full chain
        assert!(by("body_d").after.bytes < by("body_d").before.bytes);
        assert!(by("body_d").after.note.as_deref().unwrap().contains("512x512 -> 256x256"));
        for n in ["decal", "body_n"] {
            assert!(!by(n).changed(), "{n}");
            assert_eq!(get(&out, n).pixel_data, get(&src, n).pixel_data, "{n} must be byte-identical");
        }
        assert_eq!(r.bytes_after, read(&out).iter().map(|t| t.pixel_data.len()).sum::<usize>());
        assert!(r.bytes_after < r.bytes_before);
        assert!(r.file_after < r.file_before);
        // names kept, content is the downscaled picture
        assert_eq!(names(&out), names(&src));
        let back = decompress_texture(&get(&out, "body_d")).unwrap();
        let want = resize(&image::RgbaImage::from_raw(512, 512, smooth(512, 512, 255)).unwrap(), 256, 256, FilterType::Lanczos3);
        assert!(mean_err(&back, want.as_raw()) < 6, "mean error {}", mean_err(&back, want.as_raw()));

        // nothing exceeds the cap: the file is rewritten byte for byte
        let r = optimize(&src, &out, &OptimizeOptions { max_size: Some(1024), ..Default::default() }).unwrap();
        assert!(r.textures.iter().all(|c| !c.changed()));
        assert_eq!(fs::read(&out).unwrap(), fs::read(&src).unwrap());
        assert_eq!((r.bytes_before, r.file_before), (r.bytes_after, r.file_after));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn optimize_never_upscales() {
        // 1300x500 under a 1024 cap: the pipeline's round-to-pow2 would pick 1024x512, which is taller than the source
        assert_eq!(pipeline::target_size(1300, 500, &Options { max_size: Some(1024), ..Default::default() }), (1024, 512));
        assert_eq!(capped(1300, 500, 1024, Pow2::Round), (1024, 256));
        assert_eq!(capped(1300, 500, 1024, Pow2::Up), (1024, 256));
        assert_eq!(capped(1300, 500, 1024, Pow2::Keep), (1024, 396));
        assert_eq!(capped(4096, 2048, 1024, Pow2::Round), (1024, 512));
        assert_eq!(capped(2000, 12, 1024, Pow2::Round), (1024, 8));

        let d = tmp("noup");
        // the encoder cannot give a 1300x500 block texture a mip chain, so the source has one level
        let src = put(&d, "s.ytd", &[encode_texture("wide_d", &smooth(1300, 500, 255), 1300, 500, EncodeFormat::Bc1, Some(1)).unwrap()]);
        let out = d.join("o.ytd");
        let r = optimize(&src, &out, &OptimizeOptions { max_size: Some(1024), pow2: Pow2::Up, ..Default::default() }).unwrap();
        let t = get(&out, "wide_d");
        assert_eq!((t.width, t.height, t.levels), (1024, 256, 1));
        assert!(r.textures[0].after.bytes < r.textures[0].before.bytes);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn encodable_levels_match_the_encoder() {
        for (w, h) in [(96u32, 64u32), (24, 24), (100, 60), (64, 64), (48, 40)] {
            let n = encodable_levels(TextureFormat::DXT1, w, h).min(full_mip_count(TextureFormat::DXT1, w as u16, h as u16));
            let px = smooth(w, h, 255);
            assert!(encode_texture("x", &px, w, h, EncodeFormat::Bc1, Some(n)).is_ok(), "{w}x{h} with {n} levels");
            if n < full_mip_count(TextureFormat::DXT1, w as u16, h as u16) {
                assert!(encode_texture("x", &px, w, h, EncodeFormat::Bc1, Some(n + 1)).is_err(), "{w}x{h} with {} levels should be refused by the encoder", n + 1);
            }
        }
        assert_eq!(encodable_levels(TextureFormat::A8R8G8B8, 100, 60), u8::MAX);
        assert_eq!((encodable_levels(TextureFormat::DXT5, 1024, 500), encodable_levels(TextureFormat::DXT5, 512, 512)), (1, 8));
    }

    #[test]
    fn optimize_drops_small_mips_as_a_prefix_of_the_chain() {
        let d = tmp("mips");
        let src = put(&d, "s.ytd", &[tex("a_d", 64, 64, EncodeFormat::Bc1, smooth(64, 64, 255)), tex("tiny", 8, 8, EncodeFormat::Bc1, smooth(8, 8, 255))]);
        let out = d.join("o.ytd");
        let r = optimize(&src, &out, &OptimizeOptions { drop_mips_below: Some(16), ..Default::default() }).unwrap();
        let (a0, a1) = (get(&src, "a_d"), get(&out, "a_d"));
        assert_eq!((a0.levels, a1.levels), (5, 3)); // 64, 32, 16 stay; 8 and 4 go
        assert_eq!(a1.pixel_data.len(), 2048 + 512 + 128);
        assert_eq!(a1.pixel_data[..], a0.pixel_data[..a1.pixel_data.len()]);
        assert!(r.textures.iter().any(|c| c.before.name == "a_d" && c.changed()));
        // level 0 always stays
        let t1 = get(&out, "tiny");
        assert_eq!(t1.levels, 1);
        assert_eq!(t1.pixel_data[..], get(&src, "tiny").pixel_data[..t1.pixel_data.len()]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn optimize_format_override_and_target() {
        let d = tmp("fmt");
        let src = put(&d, "s.ytd", &[tex("a_d", 128, 128, EncodeFormat::Bc3, smooth(128, 128, 255)), tex("b_d", 128, 128, EncodeFormat::Bc3, smooth(128, 128, 200)), tex("c_d", 256, 256, EncodeFormat::Bc1, smooth(256, 256, 255))]);
        let out = d.join("o.ytd");
        let r = optimize(&src, &out, &OptimizeOptions { format_override: Some(EncodeFormat::Bc1), ..Default::default() }).unwrap();
        for n in ["a_d", "b_d"] {
            let (t0, t1) = (get(&src, n), get(&out, n));
            assert_eq!((t1.format, t1.levels, t1.width), (TextureFormat::DXT1, t0.levels, 128));
            assert_eq!(t1.pixel_data.len() * 2, t0.pixel_data.len());
        }
        let b = r.textures.iter().find(|c| c.before.name == "b_d").unwrap();
        assert!(b.after.note.as_deref().unwrap().contains("transparency lost"));
        let a = r.textures.iter().find(|c| c.before.name == "a_d").unwrap();
        assert!(!a.after.note.as_deref().unwrap().contains("transparency"));
        // already DXT1: untouched
        assert_eq!(get(&out, "c_d").pixel_data, get(&src, "c_d").pixel_data);

        // target: the 256x256 goes first (largest), then whatever is next largest; nothing grows
        let total: usize = read(&src).iter().map(|t| t.pixel_data.len()).sum();
        let target = total as u64 / 3;
        let r = optimize(&src, &out, &OptimizeOptions { target_bytes: Some(target), ..Default::default() }).unwrap();
        assert_eq!(r.target_met, Some(true));
        assert!(r.bytes_after as u64 <= target);
        assert_eq!(r.bytes_after, read(&out).iter().map(|t| t.pixel_data.len()).sum::<usize>());
        for c in &r.textures {
            assert!(c.after.width <= c.before.width && c.after.height <= c.before.height && c.after.bytes <= c.before.bytes);
        }
        // impossible target: written anyway, reported
        let r = optimize(&src, &out, &OptimizeOptions { target_bytes: Some(10), ..Default::default() }).unwrap();
        assert_eq!(r.target_met, Some(false));
        assert!(read(&out).iter().all(|t| t.width.max(t.height) >= 32));
        assert!(optimize(&src, &out, &OptimizeOptions { max_size: Some(2), ..Default::default() }).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn report_flags() {
        let d = tmp("report");
        let dup = noise(64, 64, 9, 255);
        let mut short = encode_texture("short_d", &smooth(64, 64, 255), 64, 64, EncodeFormat::Bc1, Some(2)).unwrap();
        short.name_hash = rage_joaat("short_d");
        let src = put(
            &d,
            "r.ytd",
            &[
                tex("ok_d", 64, 64, EncodeFormat::Bc1, smooth(64, 64, 255)),
                encode_texture("odd_d", &smooth(100, 60, 255), 100, 60, EncodeFormat::Bc1, Some(1)).unwrap(),
                tex("raw_d", 6, 4, EncodeFormat::Rgba8, smooth(6, 4, 255)),
                short,
                tex("flat_d", 64, 64, EncodeFormat::Bc3, smooth(64, 64, 255)),
                tex("glass_d", 64, 64, EncodeFormat::Bc3, smooth(64, 64, 180)),
                tex("wall_n", 64, 64, EncodeFormat::Bc1, noise(64, 64, 5, 255)),
                tex("good_n", 64, 64, EncodeFormat::Bc5, smooth(64, 64, 255)),
                tex("copy_a", 64, 64, EncodeFormat::Bc1, dup.clone()),
                tex("copy_b", 64, 64, EncodeFormat::Bc1, dup),
            ],
        );
        let rows = report(&src).unwrap();
        let codes = |n: &str| rows.iter().find(|r| r.name == n).unwrap().flags.iter().map(|f| f.code).collect::<Vec<_>>();
        assert_eq!(rows.len(), 10);
        assert!(codes("ok_d").is_empty());
        assert_eq!(codes("odd_d"), ["NON_POW2", "MISSING_MIPS"]);
        assert_eq!(codes("raw_d"), ["NON_POW2"]); // 6x4 uncompressed is allowed, only not a power of two
        assert_eq!(codes("short_d"), ["MISSING_MIPS"]);
        assert_eq!(codes("flat_d"), ["ALPHA_UNUSED"]);
        assert!(codes("glass_d").is_empty());
        assert_eq!(codes("wall_n"), ["NORMAL_MAP_FORMAT"]);
        assert!(codes("good_n").is_empty());
        assert_eq!(codes("copy_a"), ["DUPLICATE_CONTENT"]);
        assert!(rows.iter().find(|r| r.name == "copy_b").unwrap().flags[0].detail.contains("copy_a"));
        // sizes: DXT1 64x64 with 5 mips is 2048+512+128+32+8 stored, the same in VRAM
        let ok = rows.iter().find(|r| r.name == "ok_d").unwrap();
        assert_eq!((ok.bytes, ok.vram, ok.levels, ok.format.as_str()), (2728, 2728, 5, "DXT1"));
        let text = format_report(&rows);
        assert!(text.contains("ALPHA_UNUSED") && text.contains("10 texture(s)"), "{text}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn real_files_round_trip() {
        let Some(dir) = std::env::var_os("CRCK_EXPLORER_SAMPLES").map(PathBuf::from) else {
            eprintln!("CRCK_EXPLORER_SAMPLES not set: skipped");
            return;
        };
        let d = tmp("real");
        let mut n = 0;
        for f in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ytd"))) {
            let Ok((before, _)) = pipeline::read_ytd(&f) else { continue };
            if before.is_empty() {
                continue;
            }
            n += 1;
            let out = d.join(f.file_name().unwrap());
            merge(&[&f], &out, OnDuplicate::Error).unwrap();
            let mut a = before;
            a.sort_by_key(|t| lc(&t.name));
            let b = read(&out);
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(&b) {
                assert_eq!((&x.name, x.width, x.height, x.format, x.levels, &x.pixel_data), (&y.name, y.width, y.height, y.format, y.levels, &y.pixel_data), "{}", f.display());
            }
            report(&f).unwrap();
            optimize(&f, &out, &OptimizeOptions { max_size: Some(256), ..Default::default() }).unwrap();
        }
        eprintln!("{n} real YTD(s) round-tripped");
        fs::remove_dir_all(&d).unwrap();
    }
}
