//! Folder of images -> one YTD: automatic format, power-of-two policy, size cap, parallel encoding, report.
use anyhow::{bail, Context, Result};
use image::imageops::{resize, FilterType};
use rage_formats::{
    blocks::texture::convert_ytd_to_legacy,
    gen9::Edition,
    texture_encode::{auto_format, encode_texture, EncodeFormat},
    ytd::{parse_ytd, serialize_ytd, YtdTexture},
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

/// How to treat side lengths that are not powers of two (the game wants powers of two; block formats need multiples of 4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pow2 {
    /// nearest power of two on each side
    Round,
    Down,
    Up,
    /// leave as is (a side is only rounded up to a multiple of 4)
    Keep,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// downscale so the longest side is at most this
    pub max_size: Option<u32>,
    pub pow2: Pow2,
    /// None = automatic per image (BC1 opaque, BC3 alpha, BC5 normal-map names)
    pub format: Option<EncodeFormat>,
}

impl Default for Options {
    fn default() -> Self {
        Self { max_size: None, pow2: Pow2::Round, format: None }
    }
}

#[derive(Debug, Clone)]
pub struct TextureReport {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub levels: u8,
    /// bytes of pixel data in the YTD
    pub bytes: usize,
    pub note: Option<String>,
}

fn is_pow2(n: u32) -> bool {
    n != 0 && n & (n - 1) == 0
}

fn snap(n: u32, mode: Pow2) -> u32 {
    let n = n.max(4);
    if mode == Pow2::Keep {
        return (n + 3) / 4 * 4;
    }
    if is_pow2(n) {
        return n;
    }
    let down = 1u32 << (31 - n.leading_zeros());
    let up = down << 1;
    match mode {
        Pow2::Down => down,
        Pow2::Up => up,
        _ => {
            if n - down < up - n {
                down
            } else {
                up
            }
        }
    }
}

/// size an image ends up at under `opts`
pub fn target_size(w: u32, h: u32, opts: &Options) -> (u32, u32) {
    let (mut w, mut h) = (w, h);
    if let Some(m) = opts.max_size {
        let longest = w.max(h);
        if longest > m {
            w = ((w as u64 * m as u64 + longest as u64 / 2) / longest as u64).max(1) as u32;
            h = ((h as u64 * m as u64 + longest as u64 / 2) / longest as u64).max(1) as u32;
        }
    }
    let mut out = (snap(w, opts.pow2), snap(h, opts.pow2));
    if let Some(m) = opts.max_size {
        // snapping up must not break the cap
        while out.0.max(out.1) > m && m >= 8 {
            out = (if out.0 > m { out.0 / 2 } else { out.0 }, if out.1 > m { out.1 / 2 } else { out.1 });
        }
    }
    out
}

pub const IMAGE_EXT: [&str; 7] = ["png", "jpg", "jpeg", "tga", "bmp", "webp", "dds"];

fn encode_one(path: &Path, opts: &Options) -> Result<(YtdTexture, Option<String>)> {
    let name = path.file_stem().context("file without name")?.to_string_lossy().into_owned();
    if path.extension().map(|e| e.eq_ignore_ascii_case("dds")).unwrap_or(false) {
        let mut t = rage_formats::dds::parse_dds(&fs::read(path)?)?;
        t.name_hash = rage_formats::rage_joaat(&name.to_lowercase());
        t.name = name;
        return Ok((t, Some("DDS used as is (not resized or re-encoded)".into())));
    }
    let mut img = image::open(path).with_context(|| format!("decoding {}", path.display()))?.to_rgba8();
    let (sw, sh) = img.dimensions();
    let (tw, th) = target_size(sw, sh, opts);
    let resized = (tw, th) != (sw, sh);
    if resized {
        img = resize(&img, tw, th, FilterType::Lanczos3);
    }
    let fmt = opts.format.unwrap_or_else(|| auto_format(&name, img.as_raw()));
    let tex = encode_texture(&name, img.as_raw(), tw, th, fmt, None)?;
    Ok((tex, resized.then(|| format!("resized from {sw}x{sh}"))))
}

fn report_of(t: &YtdTexture, note: Option<String>) -> TextureReport {
    TextureReport { name: t.name.clone(), width: t.width as u32, height: t.height as u32, format: format!("{:?}", t.format), levels: t.levels, bytes: t.pixel_data.len(), note }
}

/// The images directly inside `dir`, sorted by name.
pub fn images_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|e| IMAGE_EXT.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Every image in `dir` (flat) -> one YTD at `out`. Encodes in parallel; textures come out sorted by file name.
pub fn folder_to_ytd(dir: &Path, out: &Path, opts: &Options) -> Result<Vec<TextureReport>> {
    let files = images_in(dir);
    if files.is_empty() {
        bail!("no images ({}) in {}", IMAGE_EXT.join(", "), dir.display());
    }
    files_to_ytd(&files, out, opts)
}

/// One planned conversion: these images become the YTD at `out`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtdJob {
    pub files: Vec<PathBuf>,
    pub out: PathBuf,
}

/// How dropped images are turned into YTDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtdMode {
    /// every image into one YTD
    All,
    /// every image gets its own YTD, named after it
    Each,
}

/// Every image in `sources`: image files as given, and the images of each folder and of the folders below it. In the order given,
/// folders sorted by name, each image once.
pub fn collect_images(sources: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        out.extend(images_in(dir));
        if depth >= crate::search::MAX_FOLDER_DEPTH {
            return;
        }
        let mut kids: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        kids.sort();
        for k in kids {
            walk(&k, out, depth + 1);
        }
    }
    let mut all: Vec<PathBuf> = vec![];
    for src in sources {
        if src.is_dir() {
            walk(src, &mut all, 0);
        } else if src.is_file() && src.extension().map(|e| IMAGE_EXT.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false) {
            all.push(src.clone());
        }
    }
    let mut seen = HashSet::new();
    all.retain(|p| seen.insert(p.clone()));
    all
}

/// What converting `sources` (image files and folders) makes. `All`: one YTD called `name` (default: the folder's name when a single
/// folder was given, else `textures`). `Each`: one YTD per image, named after the image. The YTDs go to `out_dir`, or next to the
/// first source (`All`) / next to each image (`Each`) when it is `None`. A name already taken gets `_2`, `_3`, ...
pub fn plan_ytds(sources: &[PathBuf], out_dir: Option<&Path>, mode: YtdMode, name: Option<&str>) -> Vec<YtdJob> {
    let files = collect_images(sources);
    if files.is_empty() {
        return vec![];
    }
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut place = |base: &Path, stem: &str| {
        let dir = out_dir.map(Path::to_path_buf).unwrap_or_else(|| base.to_path_buf());
        let mut out = dir.join(format!("{stem}.ytd"));
        let mut n = 2;
        while !taken.insert(out.clone()) {
            out = dir.join(format!("{stem}_{n}.ytd"));
            n += 1;
        }
        out
    };
    match mode {
        YtdMode::All => {
            let first = &sources[0];
            let named = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
            let single_dir = (sources.len() == 1 && first.is_dir()).then(|| first.file_name().map(|n| n.to_string_lossy().to_string())).flatten();
            let stem = named.or(single_dir).unwrap_or_else(|| "textures".to_string());
            let base = if first.is_dir() { first.parent().map(Path::to_path_buf).unwrap_or_else(|| first.clone()) } else { first.parent().map(Path::to_path_buf).unwrap_or_default() };
            let out = place(&base, &stem);
            vec![YtdJob { files, out }]
        }
        YtdMode::Each => files
            .into_iter()
            .map(|f| {
                let stem = f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "texture".into());
                let out = place(f.parent().unwrap_or(Path::new("")), &stem);
                YtdJob { files: vec![f], out }
            })
            .collect(),
    }
}

/// These image files -> one YTD at `out`. Encodes in parallel; textures come out sorted by file name.
pub fn files_to_ytd(files: &[PathBuf], out: &Path, opts: &Options) -> Result<Vec<TextureReport>> {
    let mut files: Vec<PathBuf> = files.to_vec();
    files.sort();
    if files.is_empty() {
        bail!("no images to convert");
    }
    let mut seen = HashSet::new();
    for f in &files {
        let n = f.file_stem().unwrap().to_string_lossy().to_lowercase();
        if !seen.insert(n.clone()) {
            bail!("two images produce the texture name '{n}' (names compare case-insensitively, extensions are ignored)");
        }
    }
    let results: Mutex<Vec<Option<Result<(YtdTexture, Option<String>)>>>> = Mutex::new((0..files.len()).map(|_| None).collect());
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(files.len());
    std::thread::scope(|sc| {
        for _ in 0..workers {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= files.len() {
                    break;
                }
                let r = encode_one(&files[i], opts);
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });
    let mut textures = vec![];
    let mut reports = vec![];
    for (i, r) in results.into_inner().unwrap().into_iter().enumerate() {
        let (t, note) = r.unwrap().with_context(|| files[i].display().to_string())?;
        reports.push(report_of(&t, note));
        textures.push(t);
    }
    fs::write(out, serialize_ytd(&textures)?)?;
    Ok(reports)
}

/// Textures of a YTD of either edition. A Gen9 (Enhanced) dictionary is converted to Legacy first (`rage-formats`'
/// conversion); the returned edition says what the file was.
pub fn read_ytd(path: &Path) -> Result<(Vec<YtdTexture>, Option<Edition>)> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let edition = Edition::of_file("ytd", &data)?;
    let data = if edition == Some(Edition::Gen9) { convert_ytd_to_legacy(&data).with_context(|| format!("{} is a Gen9 YTD that could not be converted", path.display()))? } else { data };
    Ok((parse_ytd(&data).with_context(|| format!("parsing {}", path.display()))?, edition))
}

/// What is in a YTD: one row per texture, plus the edition the file was written for.
pub fn ytd_info(ytd: &Path) -> Result<(Vec<TextureReport>, Option<Edition>)> {
    let (t, e) = read_ytd(ytd)?;
    Ok((t.iter().map(|t| report_of(t, None)).collect(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_sizes() {
        let o = Options::default();
        assert_eq!(target_size(600, 400, &o), (512, 512));
        assert_eq!(target_size(1000, 300, &o), (1024, 256));
        assert_eq!(target_size(512, 512, &o), (512, 512));
        assert_eq!(target_size(2, 3, &o), (4, 4));
        let m = Options { max_size: Some(1024), ..Default::default() };
        assert_eq!(target_size(4096, 2048, &m), (1024, 512));
        assert_eq!(target_size(1500, 1500, &m), (1024, 1024));
        let up = Options { pow2: Pow2::Up, ..Default::default() };
        assert_eq!(target_size(600, 400, &up), (1024, 512));
        let keep = Options { pow2: Pow2::Keep, ..Default::default() };
        assert_eq!(target_size(601, 399, &keep), (604, 400));
    }

    #[test]
    fn dropped_images_make_one_ytd_or_one_each() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_plan_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let png = |p: &Path| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255])).save(p).unwrap();
        };
        png(&d.join("paint").join("livery_a.png"));
        png(&d.join("paint").join("livery_b.png"));
        png(&d.join("paint").join("sub").join("x.png"));
        fs::create_dir_all(d.join("empty")).unwrap();
        png(&d.join("loose1.png"));
        fs::write(d.join("notes.txt"), "ignored").unwrap();
        // all into one: the folder's name when a single folder was dropped, the images of subfolders included
        let jobs = plan_ytds(&[d.join("paint")], None, YtdMode::All, None);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].out, d.join("paint.ytd"));
        assert_eq!(jobs[0].files.len(), 3);
        // a chosen name, folder plus loose file, one output folder
        let elsewhere = d.join("out");
        let jobs = plan_ytds(&[d.join("paint"), d.join("loose1.png"), d.join("notes.txt")], Some(&elsewhere), YtdMode::All, Some("my_pack"));
        assert_eq!((jobs.len(), jobs[0].files.len(), jobs[0].out.clone()), (1, 4, elsewhere.join("my_pack.ytd")));
        // each image its own: named after the image, next to it; a repeated name is numbered
        let jobs = plan_ytds(&[d.join("paint"), d.join("empty")], None, YtdMode::Each, None);
        let outs: Vec<String> = jobs.iter().map(|j| j.out.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(outs, ["livery_a.ytd", "livery_b.ytd", "x.ytd"]);
        assert_eq!(jobs[2].out.parent().unwrap(), d.join("paint").join("sub"));
        let same = d.join("paint").join("livery_a.png");
        let jobs = plan_ytds(&[same.clone(), same], Some(&elsewhere), YtdMode::Each, None);
        assert_eq!(jobs.len(), 1, "an image given twice is converted once");
        assert!(plan_ytds(&[d.join("empty")], None, YtdMode::All, None).is_empty());
        // and a plan runs
        fs::create_dir_all(&elsewhere).unwrap();
        let all = plan_ytds(&[d.join("paint")], Some(&elsewhere), YtdMode::All, None);
        let rep = files_to_ytd(&all[0].files, &all[0].out, &Options::default()).unwrap();
        assert_eq!(rep.len(), 3);
        assert_eq!(ytd_info(&all[0].out).unwrap().0.len(), 3);
        // two images with one name cannot share a YTD
        png(&d.join("paint").join("sub").join("livery_a.png"));
        let all = plan_ytds(&[d.join("paint")], Some(&elsewhere), YtdMode::All, Some("clash"));
        assert!(files_to_ytd(&all[0].files, &all[0].out, &Options::default()).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_pipeline_end_to_end() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_pipe_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let grad = |w: u32, h: u32, a: u8| image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 90, a]));
        grad(300, 200, 255).save(d.join("body_d.png")).unwrap();
        grad(64, 64, 128).save(d.join("decal.png")).unwrap();
        grad(64, 64, 255).save(d.join("body_n.png")).unwrap();
        fs::write(d.join("notes.txt"), "ignored").unwrap();
        let out = d.join("out.ytd");
        let rep = folder_to_ytd(&d, &out, &Options::default()).unwrap();
        assert_eq!(rep.len(), 3);
        let by = |n: &str| rep.iter().find(|r| r.name == n).unwrap().clone();
        assert_eq!((by("body_d").width, by("body_d").height), (256, 256));
        assert_eq!(by("body_d").format, "DXT1");
        assert_eq!(by("decal").format, "DXT5");
        assert_eq!(by("body_n").format, "ATI2");
        assert_eq!(ytd_info(&out).unwrap().0.len(), 3);
        // duplicate names (different extensions) are rejected
        image::DynamicImage::ImageRgba8(grad(64, 64, 255)).to_rgb8().save(d.join("decal.jpg")).unwrap();
        assert!(folder_to_ytd(&d, &out, &Options::default()).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
