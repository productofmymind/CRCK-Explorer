//! PNG/JPG/... -> BCn texture inside a YTD, with mips, optional resize and automatic format choice.
use anyhow::{bail, Context, Result};
use image::imageops::{resize, FilterType};
use rage_formats::{
    texture_encode::{auto_format, encode_texture},
    ytd::serialize_ytd,
};
pub use rage_formats::texture_encode::EncodeFormat;
use std::{fs, path::Path};

pub struct Imported {
    pub name: String,
    pub replaced: bool,
    pub width: u32,
    pub height: u32,
    pub warning: Option<String>,
}

fn pow2(n: u32) -> bool {
    n != 0 && n & (n - 1) == 0
}

/// Replace (by file name) or add the texture `png` in `ytd_in`, write `out`.
/// `format`: None = automatic (BC1 opaque, BC3 alpha, BC5 normal-map names); `size`: resize with Lanczos.
/// Adds a picture to a YTD as a texture named after the file (a texture of that name is replaced). A DDS goes in as it is: its
/// pixel format and mip levels are kept, nothing is re-encoded.
pub fn import_image(ytd_in: &Path, png: &Path, out: &Path, format: Option<EncodeFormat>, size: Option<(u32, u32)>) -> Result<Imported> {
    let (mut textures, _) = crate::pipeline::read_ytd(ytd_in).with_context(|| format!("reading {}", ytd_in.display()))?;
    let name = png.file_stem().context("png has no file name")?.to_string_lossy().into_owned();
    let is_dds = png.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds"));
    let tex = if is_dds {
        if format.is_some() || size.is_some() {
            bail!("a DDS is added as it is: a format or size only applies to PNG, JPG and the other image files");
        }
        let mut t = rage_formats::dds::parse_dds(&fs::read(png)?).with_context(|| format!("reading {}", png.display()))?;
        t.name = name.clone();
        t.name_hash = 0; // hashed from the name when written
        t
    } else {
        let mut img = image::open(png).with_context(|| format!("decoding {}", png.display()))?.to_rgba8();
        if let Some((w, h)) = size {
            if w == 0 || h == 0 {
                bail!("size must be non-zero");
            }
            img = resize(&img, w, h, FilterType::Lanczos3);
        }
        let (w, h) = img.dimensions();
        let fmt = format.unwrap_or_else(|| auto_format(&name, img.as_raw()));
        // mip chain: the crate's full chain, which stops at 4x4 for block formats (the C# importer went down to 1x1; unverified which the game prefers)
        encode_texture(&name, img.as_raw(), w, h, fmt, None)?
    };
    let (w, h) = (tex.width as u32, tex.height as u32);
    let warning = (!pow2(w) || !pow2(h)).then(|| format!("{w}x{h} is not power-of-two; the game may reject or mishandle it. Pass a size to resize."));
    let idx = textures.iter().position(|t| t.name.eq_ignore_ascii_case(&name));
    match idx {
        Some(i) => textures[i] = tex,
        None => textures.push(tex),
    }
    fs::write(out, serialize_ytd(&textures)?)?;
    Ok(Imported { name, replaced: idx.is_some(), width: w, height: h, warning })
}

// ---- replace / export one texture of a YTD -------------------------------------------------------------------------------

/// What to keep from the texture being replaced when the new image comes from a PNG/JPG/...: its pixel format and its size.
/// (A DDS is always used as it is.)
#[derive(Clone, Copy, Debug)]
pub struct ReplaceOpts {
    pub keep_format: bool,
    pub keep_size: bool,
}

impl Default for ReplaceOpts {
    fn default() -> Self {
        Self { keep_format: true, keep_size: false }
    }
}

pub(crate) fn encode_format_of(f: rage_formats::ytd::TextureFormat) -> Option<EncodeFormat> {
    use rage_formats::ytd::TextureFormat as T;
    Some(match f {
        T::DXT1 => EncodeFormat::Bc1,
        T::DXT5 => EncodeFormat::Bc3,
        T::ATI1 => EncodeFormat::Bc4,
        T::ATI2 => EncodeFormat::Bc5,
        T::BC7 => EncodeFormat::Bc7,
        T::A8R8G8B8 => EncodeFormat::Rgba8,
        _ => return None,
    })
}

/// Replace the texture called `name` in `ytd_in` with `src` (a .dds used as is, or any image), write the result to `out`.
/// A Gen9 YTD is converted to Legacy on the way (the output is always Legacy). Returns a one-line description.
pub fn replace_texture(ytd_in: &Path, name: &str, src: &Path, out: &Path, opts: ReplaceOpts) -> Result<String> {
    let (mut textures, edition) = crate::pipeline::read_ytd(ytd_in)?;
    let idx = textures.iter().position(|t| t.name.eq_ignore_ascii_case(name)).with_context(|| format!("no texture named '{name}' in {}", ytd_in.display()))?;
    let old = textures[idx].clone();
    let is_dds = src.extension().map(|e| e.eq_ignore_ascii_case("dds")).unwrap_or(false);
    let mut new = if is_dds {
        rage_formats::dds::parse_dds(&fs::read(src)?).with_context(|| format!("reading {}", src.display()))?
    } else {
        let mut img = image::open(src).with_context(|| format!("decoding {}", src.display()))?.to_rgba8();
        if opts.keep_size {
            let (w, h) = (old.width as u32, old.height as u32);
            if img.dimensions() != (w, h) {
                img = resize(&img, w, h, FilterType::Lanczos3);
            }
        }
        let (w, h) = img.dimensions();
        let fmt = if opts.keep_format { encode_format_of(old.format) } else { None }.unwrap_or_else(|| auto_format(&old.name, img.as_raw()));
        encode_texture(&old.name, img.as_raw(), w, h, fmt, None)?
    };
    new.name = old.name.clone();
    new.name_hash = old.name_hash;
    let msg = format!("{}: {}x{} {:?} -> {}x{} {:?}{}", old.name, old.width, old.height, old.format, new.width, new.height, new.format, if edition == Some(rage_formats::gen9::Edition::Gen9) { " (Gen9 file written as Legacy)" } else { "" });
    textures[idx] = new;
    fs::write(out, serialize_ytd(&textures)?)?;
    Ok(msg)
}

/// A picture file ready to show: pixels plus a one-line description.
pub struct ImagePreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// e.g. `Dxt5, 11 mips` (DDS) or `PNG`
    pub info: String,
}

/// Read a `.dds` (any block format the game uses) or an ordinary image file (png, jpg, tga, bmp, webp) for display.
pub fn preview_image(bytes: &[u8], name: &str) -> Result<ImagePreview> {
    let is_dds = name.rsplit('.').next().map(|e| e.eq_ignore_ascii_case("dds")).unwrap_or(false);
    if is_dds {
        let t = rage_formats::dds::parse_dds(bytes).with_context(|| format!("reading {name}"))?;
        let (width, height, rgba) = crate::model::texture_rgba(&t)?;
        let info = format!("{:?}, {} mip{}", t.format, t.levels, if t.levels == 1 { "" } else { "s" });
        Ok(ImagePreview { width, height, rgba, info })
    } else {
        let img = image::load_from_memory(bytes).with_context(|| format!("reading {name}"))?.into_rgba8();
        let info = image::guess_format(bytes).map(|f| format!("{f:?}")).unwrap_or_default();
        Ok(ImagePreview { width: img.width(), height: img.height(), rgba: img.into_raw(), info })
    }
}

/// Save one texture as `.dds` (exact stored pixels) or any image format the `image` crate writes (png, jpg, ...).
pub fn export_texture(ytd: &Path, name: &str, out: &Path) -> Result<()> {
    let (textures, _) = crate::pipeline::read_ytd(ytd)?;
    let t = textures.iter().find(|t| t.name.eq_ignore_ascii_case(name)).with_context(|| format!("no texture named '{name}' in {}", ytd.display()))?;
    if out.extension().map(|e| e.eq_ignore_ascii_case("dds")).unwrap_or(false) {
        fs::write(out, t.to_dds())?;
    } else {
        rage_formats::texture_utils::to_rgba_image(t)?.save(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rage_formats::texture_utils::decompress_texture;
    use rage_formats::texture_encode::encode_texture;

    #[test]
    fn gradient_survives_encode_decode() {
        let (w, h) = (64u32, 64u32);
        let px: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 4) as u8, (y * 4) as u8, 128, 255])).collect();
        let t = encode_texture("g", &px, w, h, EncodeFormat::Bc7, None).unwrap();
        assert_eq!(t.levels, 5); // crate default: chain stops at 4x4 for block formats
        let back = decompress_texture(&t).unwrap();
        let err: u64 = px.iter().zip(&back).map(|(a, b)| a.abs_diff(*b) as u64).sum::<u64>() / px.len() as u64;
        assert!(err < 4, "mean abs error {err}");
    }

    #[test]
    fn replace_and_export_one_texture() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_repl_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("imgs")).unwrap();
        let grad = |w: u32, h: u32, a: u8| image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 40, a]));
        grad(64, 64, 255).save(d.join("imgs/a_d.png")).unwrap();
        grad(32, 32, 128).save(d.join("imgs/b_d.png")).unwrap();
        let ytd = d.join("t.ytd");
        crate::pipeline::folder_to_ytd(&d.join("imgs"), &ytd, &crate::pipeline::Options::default()).unwrap();
        // a 128x128 opaque image replaces the 64x64 DXT1 one: format kept, size taken from the new image
        let new_png = d.join("new.png");
        grad(128, 128, 255).save(&new_png).unwrap();
        let out = d.join("t2.ytd");
        replace_texture(&ytd, "A_D", &new_png, &out, ReplaceOpts::default()).unwrap();
        let (rows, _) = crate::pipeline::ytd_info(&out).unwrap();
        let a = rows.iter().find(|r| r.name == "a_d").unwrap();
        assert_eq!((a.width, a.height, a.format.as_str()), (128, 128, "DXT1"));
        assert_eq!(rows.iter().find(|r| r.name == "b_d").unwrap().format, "DXT5");
        // keep_size scales to the old size
        let out_k = d.join("t3.ytd");
        replace_texture(&ytd, "a_d", &new_png, &out_k, ReplaceOpts { keep_format: true, keep_size: true }).unwrap();
        assert_eq!(crate::pipeline::ytd_info(&out_k).unwrap().0.iter().find(|r| r.name == "a_d").unwrap().width, 64);
        // DDS round trip: export b_d, replace a_d with it, the DDS is used as is
        let dds = d.join("b.dds");
        export_texture(&ytd, "b_d", &dds).unwrap();
        let out_d = d.join("t4.ytd");
        replace_texture(&ytd, "a_d", &dds, &out_d, ReplaceOpts::default()).unwrap();
        let (rows, _) = crate::pipeline::ytd_info(&out_d).unwrap();
        let a = rows.iter().find(|r| r.name == "a_d").unwrap();
        assert_eq!((a.width, a.height, a.format.as_str()), (32, 32, "DXT5"));
        // PNG export decodes
        let png = d.join("b.png");
        export_texture(&ytd, "b_d", &png).unwrap();
        assert_eq!(image::open(&png).unwrap().width(), 32);
        assert!(replace_texture(&ytd, "nope", &new_png, &out, ReplaceOpts::default()).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_dds_is_added_as_it_is() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_add_dds_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("imgs")).unwrap();
        let grad = |w: u32, h: u32, a: u8| image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 40, a]));
        grad(64, 64, 255).save(d.join("imgs/a_d.png")).unwrap();
        grad(32, 32, 128).save(d.join("imgs/b_d.png")).unwrap();
        let ytd = d.join("t.ytd");
        crate::pipeline::folder_to_ytd(&d.join("imgs"), &ytd, &crate::pipeline::Options::default()).unwrap();
        // b_d out as mesh.dds (DXT5, 32x32 with its mips), then added: a third texture, named after the file, pixels unchanged
        let dds = d.join("mesh.dds");
        export_texture(&ytd, "b_d", &dds).unwrap();
        let out = d.join("t2.ytd");
        let r = import_image(&ytd, &dds, &out, None, None).unwrap();
        assert_eq!((r.name.as_str(), r.replaced, r.width, r.height), ("mesh", false, 32, 32));
        let (textures, _) = crate::pipeline::read_ytd(&out).unwrap();
        let (b, mesh) = (textures.iter().find(|t| t.name == "b_d").unwrap(), textures.iter().find(|t| t.name == "mesh").unwrap());
        assert_eq!(textures.len(), 3);
        assert_eq!((mesh.format, mesh.levels, &mesh.pixel_data), (b.format, b.levels, &b.pixel_data), "the DDS is not re-encoded");
        // a format or a size cannot apply to a DDS
        assert!(import_image(&ytd, &dds, &out, None, Some((16, 16))).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
