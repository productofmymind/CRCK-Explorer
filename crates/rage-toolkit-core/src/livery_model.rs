//! A livery model (`.yft`) from a picture: the mod-kit livery files the game uses (`polb2_livery1.yft`, ...) are a small
//! fragment, a 4-vertex quad with a `vehicle_paint3` shader whose `DiffuseSampler2` is the livery, carried as a texture
//! embedded in the file. A new livery is the vehicle's existing livery file with another picture in that texture, the way it
//! is done by hand in classic modding tools (copy `livery1.yft`, replace the embedded texture).
//!
//! The file is changed in place: only the texture's pixels are written over, so the layout of the resource (its pages, every
//! pointer) stays exactly as the template's. The picture is therefore made the same size, pixel format and number of mips as
//! the template's texture (a `.dds` that already is is used as it is). The texture keeps its name, as the shader asks for it.
//! The result is read back and compared before it is written.
use crate::model;
use anyhow::{bail, Context, Result};
use image::imageops::{resize, FilterType};
use rage_formats::{
    texture_encode::encode_texture,
    ytd::{TextureFormat, YtdTexture},
};
use std::{fs, path::Path};

const SYSTEM_BASE: u64 = 0x5000_0000;
const GRAPHICS_BASE: u64 = 0x6000_0000;

/// The embedded texture the livery shader shows (`DiffuseSampler2`), else the only embedded texture.
fn livery_texture(template: &Path) -> Result<YtdTexture> {
    let reports = model::inspect(template)?;
    let embedded: Vec<String> = reports.iter().flat_map(|r| r.embedded_textures.iter().cloned()).collect();
    let wanted = reports.iter().flat_map(|r| &r.shaders).flat_map(|s| &s.textures).find(|(p, n)| p == "DiffuseSampler2" && embedded.iter().any(|e| e.eq_ignore_ascii_case(n))).map(|(_, n)| n.clone());
    let name = match (wanted, embedded.as_slice()) {
        (Some(n), _) => n,
        (None, [one]) => one.clone(),
        (None, []) => bail!("{} has no embedded texture: choose the vehicle's livery file (like polb2_livery1.yft) as the template", template.display()),
        (None, many) => bail!("{} has several embedded textures ({}) and none is the livery (DiffuseSampler2)", template.display(), many.join(", ")),
    };
    model::load(template)?
        .into_iter()
        .filter_map(|e| e.drawable.shader_group)
        .flat_map(|g| g.textures)
        .find(|t| t.name.eq_ignore_ascii_case(&name))
        .with_context(|| format!("texture {name} not found in {}", template.display()))
}

/// The picture as a texture shaped exactly like `old`: same size, pixel format and mips (so the same number of bytes).
fn shaped_like(old: &YtdTexture, picture: &Path) -> Result<(YtdTexture, bool)> {
    let is_dds = picture.extension().is_some_and(|e| e.eq_ignore_ascii_case("dds"));
    if is_dds {
        let t = rage_formats::dds::parse_dds(&fs::read(picture)?).with_context(|| format!("reading {}", picture.display()))?;
        if (t.width, t.height, t.format, t.levels) == (old.width, old.height, old.format, old.levels) && t.pixel_data.len() == old.pixel_data.len() {
            return Ok((t, false));
        }
    }
    let mut img = if is_dds {
        let t = rage_formats::dds::parse_dds(&fs::read(picture)?)?;
        let (w, h, rgba) = model::texture_rgba(&t)?;
        image::RgbaImage::from_raw(w, h, rgba).context("bad picture size")?
    } else {
        image::open(picture).with_context(|| format!("decoding {}", picture.display()))?.to_rgba8()
    };
    let (w, h) = (old.width as u32, old.height as u32);
    if img.dimensions() != (w, h) {
        img = resize(&img, w, h, FilterType::Lanczos3);
    }
    let fmt = crate::texture::encode_format_of(old.format).with_context(|| format!("the template's texture is {:?}, which CRCK Explorer cannot encode", old.format))?;
    let t = encode_texture(&old.name, img.as_raw(), w, h, fmt, Some(old.levels))?;
    if t.pixel_data.len() != old.pixel_data.len() {
        bail!("the new texture is {} bytes, the template's {} (mips {} vs {})", t.pixel_data.len(), old.pixel_data.len(), t.levels, old.levels);
    }
    Ok((t, true))
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

/// Where the texture's pixels are in the graphics section: found through its header (name pointer at +0x28, size at +0x50,
/// mips at +0x5D, data pointer at +0x70) and accepted only when the bytes there are exactly the old pixels.
fn pixels_offset(sys: &[u8], gfx: &[u8], old: &YtdTexture) -> Result<usize> {
    let name = format!("{}\0", old.name);
    let mut found = None;
    for (at, _) in sys.windows(name.len()).enumerate().filter(|(_, w)| w.eq_ignore_ascii_case(name.as_bytes())) {
        let name_va = SYSTEM_BASE + at as u64;
        for h in (0..sys.len().saturating_sub(0x90)).step_by(16) {
            if u64_at(sys, h + 0x28) != Some(name_va) || u16_at(sys, h + 0x50) != Some(old.width) || u16_at(sys, h + 0x52) != Some(old.height) || sys.get(h + 0x5D) != Some(&old.levels) {
                continue;
            }
            let Some(data) = u64_at(sys, h + 0x70).filter(|d| *d >= GRAPHICS_BASE) else { continue };
            let off = (data - GRAPHICS_BASE) as usize;
            if gfx.get(off..off + old.pixel_data.len()) == Some(old.pixel_data.as_slice()) {
                if found.is_some_and(|f| f != off) {
                    bail!("the texture's pixels were found twice: not changing the file");
                }
                found = Some(off);
                let _ = u32_at(sys, h + 0x58);
            }
        }
    }
    found.context("the texture's pixels were not found where its header says")
}

/// Write `out`: the livery model `template` with `picture` (`.dds` or any image) as its livery texture. Returns a line for
/// the user.
pub fn livery_yft(template: &Path, picture: &Path, out: &Path) -> Result<String> {
    let bytes = fs::read(template).with_context(|| format!("reading {}", template.display()))?;
    if bytes.len() < 16 || &bytes[0..4] != b"RSC7" {
        bail!("{} is not a GTA V resource (RSC7)", template.display());
    }
    let old = livery_texture(template)?;
    if !matches!(old.format, TextureFormat::DXT1 | TextureFormat::DXT5 | TextureFormat::ATI1 | TextureFormat::ATI2 | TextureFormat::BC7 | TextureFormat::A8R8G8B8) {
        bail!("the template's livery texture is {:?}: not a format CRCK Explorer can write", old.format);
    }
    let (new, converted) = shaped_like(&old, picture)?;
    let (version, sys_flags, gfx_flags) = (u32_at(&bytes, 4).unwrap(), u32_at(&bytes, 8).unwrap(), u32_at(&bytes, 12).unwrap());
    let (sys, mut gfx) = rage_formats::prepare_rsc7(&bytes).with_context(|| format!("unpacking {}", template.display()))?;
    let at = pixels_offset(&sys, &gfx, &old)?;
    gfx[at..at + new.pixel_data.len()].copy_from_slice(&new.pixel_data);
    let written = rage_formats::build_rsc7_with_flags(version, sys_flags, &sys, gfx_flags, &gfx);
    // read it back: the livery texture must now be the new pixels, the rest of the model as it was
    let tmp = out.with_extension("rage-toolkit-check.yft");
    fs::write(&tmp, &written)?;
    let check = livery_texture(&tmp).and_then(|t| {
        let same_model = model::inspect(&tmp)?.iter().map(|r| (r.lods.len(), r.shaders.len())).eq(model::inspect(template)?.iter().map(|r| (r.lods.len(), r.shaders.len())));
        if t.pixel_data != new.pixel_data || !same_model {
            bail!("the written file does not read back as expected");
        }
        Ok(())
    });
    let _ = fs::remove_file(&tmp);
    check?;
    fs::write(out, &written).with_context(|| format!("writing {}", out.display()))?;
    Ok(format!(
        "{} written: texture {} is now {}{}",
        out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        old.name,
        picture.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        if converted { format!(" (made {}x{} {:?} with {} mips, like the template's)", old.width, old.height, old.format, old.levels) } else { String::new() }
    ))
}

/// The next free `<base>_livery<n>.yft` (or `<base>_liv<n>.yft`) beside `template`, for the Save as box.
pub fn next_name(template: &Path) -> Option<std::path::PathBuf> {
    let stem = template.file_stem()?.to_string_lossy().into_owned();
    let digits = stem.chars().rev().take_while(|c| c.is_ascii_digit()).count();
    let (base, num) = stem.split_at(stem.len() - digits);
    let mut n: u32 = num.parse().unwrap_or(1);
    loop {
        n += 1;
        let p = template.with_file_name(format!("{base}{n}.yft"));
        if !p.exists() {
            return Some(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_livery_name_counts_up_past_the_ones_there() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_livname_{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("polb2_livery2.yft"), b"x").unwrap();
        assert_eq!(next_name(&d.join("polb2_livery1.yft")), Some(d.join("polb2_livery3.yft")));
        assert_eq!(next_name(&d.join("car_liv.yft")), Some(d.join("car_liv2.yft")));
        fs::remove_dir_all(d).unwrap();
    }
}

#[cfg(test)]
mod game_tests {
    use super::*;

    /// needs the installed game: a real livery file as the template, a picture of another size as the new livery
    #[test]
    fn a_game_livery_file_gets_a_new_picture_and_keeps_everything_else() {
        let Ok(g) = crate::game::Game::open(None) else {
            eprintln!("skipped: GTA V not found");
            return;
        };
        let Some(f) = g.find("polb2_livery1.yft").pop() else { return };
        let d = std::env::temp_dir().join(format!("rage_toolkit_livyft_{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let template = d.join("polb2_livery1.yft");
        fs::write(&template, g.read(&f.path).unwrap()).unwrap();
        let old = livery_texture(&template).unwrap();
        // a 300x200 picture: not the template's size, so it is made like the template's texture
        let pic = d.join("mine.png");
        image::RgbaImage::from_fn(300, 200, |x, y| image::Rgba([x as u8, y as u8, 200, 255])).save(&pic).unwrap();
        let out = d.join("polb2_livery9.yft");
        let msg = livery_yft(&template, &pic, &out).unwrap();
        assert!(msg.contains("like the template"), "{msg}");
        let new = livery_texture(&out).unwrap();
        assert_eq!((new.name.as_str(), new.width, new.height, new.format, new.levels), (old.name.as_str(), old.width, old.height, old.format, old.levels));
        assert_ne!(new.pixel_data, old.pixel_data);
        assert_eq!(fs::metadata(&out).unwrap().len() > 0, true);
        fs::remove_dir_all(d).unwrap();
    }
}
