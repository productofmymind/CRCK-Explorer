//! Image → texture encoding: RGBA8 pixels to a block-compressed (or plain
//! 32-bit) [`YtdTexture`] with a full mip chain, ready for a `.ytd` or a
//! `.dds`. Built on the pure-Rust `rusty_dds` encoder.
use anyhow::{bail, Result};
use rusty_dds::{DecodeContent, Dds, EncodeLayout};

use crate::ytd::{full_mip_count, mip_chain_size, stride_for, to_ytd_layout, TextureFormat, YtdTexture};

/// The formats a texture can be encoded to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeFormat {
    /// DXT1: 4 bpp, 1-bit alpha at most. The default for opaque images.
    Bc1,
    /// DXT5: 8 bpp with smooth alpha. The default for images with alpha.
    Bc3,
    /// ATI1: single channel, 4 bpp (the red channel is kept).
    Bc4,
    /// ATI2: two channels, 8 bpp; what the game uses for normal maps.
    Bc5,
    /// 8 bpp, best quality of the block formats.
    Bc7,
    /// A8R8G8B8, uncompressed.
    Rgba8,
}

impl EncodeFormat {
    /// Accepts the BC names and the game's own (`dxt1`, `dxt5`, `ati2`...),
    /// case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "bc1" | "dxt1" => Self::Bc1,
            "bc3" | "dxt5" => Self::Bc3,
            "bc4" | "ati1" => Self::Bc4,
            "bc5" | "ati2" => Self::Bc5,
            "bc7" => Self::Bc7,
            "rgba" | "rgba8" | "a8r8g8b8" | "none" => Self::Rgba8,
            _ => return None,
        })
    }

    pub fn texture_format(self) -> TextureFormat {
        match self {
            Self::Bc1 => TextureFormat::DXT1,
            Self::Bc3 => TextureFormat::DXT5,
            Self::Bc4 => TextureFormat::ATI1,
            Self::Bc5 => TextureFormat::ATI2,
            Self::Bc7 => TextureFormat::BC7,
            Self::Rgba8 => TextureFormat::A8R8G8B8,
        }
    }

    fn content(self) -> DecodeContent {
        match self {
            Self::Bc1 => DecodeContent::Bc1,
            Self::Bc3 => DecodeContent::Bc3,
            Self::Bc4 => DecodeContent::Bc4UNorm,
            Self::Bc5 => DecodeContent::Bc5UNorm,
            Self::Bc7 => DecodeContent::Bc7,
            Self::Rgba8 => DecodeContent::Bgra8,
        }
    }
}

impl std::fmt::Display for EncodeFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bc1 => "bc1",
            Self::Bc3 => "bc3",
            Self::Bc4 => "bc4",
            Self::Bc5 => "bc5",
            Self::Bc7 => "bc7",
            Self::Rgba8 => "rgba8",
        })
    }
}

/// Whether a texture name marks a normal map (`_n`, `_nrm`, `_normal`,
/// `_nm`, with or without a trailing digit as in `_n2`).
pub fn is_normal_map_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.trim_end_matches(|c: char| c.is_ascii_digit());
    ["_n", "_nrm", "_normal", "_nm", "_bump"].iter().any(|s| stem.ends_with(s))
}

/// The format to use when none is asked for: BC5 for a normal map (by
/// name), BC3 when any pixel is not fully opaque, BC1 otherwise.
pub fn auto_format(name: &str, rgba: &[u8]) -> EncodeFormat {
    if is_normal_map_name(name) {
        return EncodeFormat::Bc5;
    }
    if rgba.chunks_exact(4).any(|p| p[3] != 255) {
        EncodeFormat::Bc3
    } else {
        EncodeFormat::Bc1
    }
}

/// Encodes tightly packed RGBA8 pixels (`width * height * 4` bytes) into a
/// texture called `name`. `levels` is the mip count wanted; `None` or a
/// count beyond what the size allows gives the full chain (down to 4x4 for
/// block formats, 1x1 otherwise). Block formats need both dimensions to be
/// multiples of 4.
pub fn encode_texture(name: &str, rgba: &[u8], width: u32, height: u32, format: EncodeFormat, levels: Option<u8>) -> Result<YtdTexture> {
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        bail!("{width}x{height} is not a usable texture size");
    }
    if rgba.len() != (width * height * 4) as usize {
        bail!("{} bytes of RGBA for {width}x{height}; expected {}", rgba.len(), width * height * 4);
    }
    let tex_format = format.texture_format();
    if tex_format.is_block_compressed() && (width % 4 != 0 || height % 4 != 0) {
        bail!("{width}x{height} cannot be {format}-compressed: both sides must be multiples of 4 (resize or pad the image)");
    }
    let (w, h) = (width as u16, height as u16);
    let max_levels = full_mip_count(tex_format, w, h);
    let levels = levels.map_or(max_levels, |n| n.clamp(1, max_levels));

    let layout = EncodeLayout::flat_2d(format.content(), width, height).with_mips(levels as u32);
    let dds = Dds::encode_from_rgba8(rgba, layout).map_err(|e| anyhow::anyhow!("{format} encoding failed: {e}"))?;
    let expected = mip_chain_size(tex_format, w, h, levels);
    let data = dds.get_data(0).map_err(|e| anyhow::anyhow!("{e}"))?;
    if data.len() < expected {
        bail!("encoder produced {} bytes, expected {}", data.len(), expected);
    }

    Ok(YtdTexture {
        name: name.to_string(),
        name_hash: crate::rage_joaat(&name.to_lowercase()),
        width: w,
        height: h,
        depth: 1,
        format: tex_format,
        levels,
        stride: stride_for(tex_format, w, h),
        pixel_data: to_ytd_layout(tex_format, w, h, stride_for(tex_format, w, h), levels, &data[..expected]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128, alpha]);
            }
        }
        px
    }

    #[test]
    fn auto_format_picks_by_name_then_alpha() {
        assert_eq!(auto_format("wall_n", &gradient(4, 4, 255)), EncodeFormat::Bc5);
        assert_eq!(auto_format("Wall_N2", &gradient(4, 4, 255)), EncodeFormat::Bc5);
        assert_eq!(auto_format("wall_normal", &gradient(4, 4, 255)), EncodeFormat::Bc5);
        assert_eq!(auto_format("wall_d", &gradient(4, 4, 255)), EncodeFormat::Bc1);
        assert_eq!(auto_format("wall_d", &gradient(4, 4, 200)), EncodeFormat::Bc3);
        assert_eq!(auto_format("nation", &gradient(4, 4, 255)), EncodeFormat::Bc1);
    }

    #[test]
    fn full_chain_sizes_match_the_games_arithmetic() {
        let tex = encode_texture("lights", &gradient(512, 256, 255), 512, 256, EncodeFormat::Bc3, None).unwrap();
        assert_eq!(tex.levels, 7);
        assert_eq!(tex.format, TextureFormat::DXT5);
        assert_eq!(tex.stride, 512);
        assert_eq!(tex.pixel_data.len(), 131072 + 32768 + 8192 + 2048 + 512 + 128 + 32);
        assert_eq!(tex.name_hash, crate::rage_joaat("lights"));

        let tex = encode_texture("x", &gradient(8, 8, 255), 8, 8, EncodeFormat::Rgba8, None).unwrap();
        assert_eq!(tex.levels, 4);
        assert_eq!(tex.pixel_data.len(), 256 + 64 + 16 + 4);
    }

    #[test]
    fn requested_levels_are_clamped_to_what_fits() {
        let tex = encode_texture("x", &gradient(16, 16, 255), 16, 16, EncodeFormat::Bc1, Some(9)).unwrap();
        assert_eq!(tex.levels, 3);
        let tex = encode_texture("x", &gradient(16, 16, 255), 16, 16, EncodeFormat::Bc7, Some(1)).unwrap();
        assert_eq!(tex.levels, 1);
        assert_eq!(tex.pixel_data.len(), 16 * 16);
    }

    #[test]
    fn encoded_pixels_decode_back_close_to_the_source() {
        let src = gradient(32, 32, 255);
        for format in [EncodeFormat::Bc1, EncodeFormat::Bc3, EncodeFormat::Bc7, EncodeFormat::Rgba8] {
            let tex = encode_texture("g", &src, 32, 32, format, Some(1)).unwrap();
            let back = crate::decompress_texture(&tex).unwrap();
            let max_err = src.chunks_exact(4).zip(back.chunks_exact(4))
                .flat_map(|(a, b)| a[..3].iter().zip(&b[..3]).map(|(x, y)| (*x as i32 - *y as i32).abs()))
                .max().unwrap();
            let tolerance = if format == EncodeFormat::Rgba8 { 0 } else { 24 };
            assert!(max_err <= tolerance, "{format}: max channel error {max_err}");
        }
    }

    #[test]
    fn block_formats_reject_sizes_that_are_not_multiples_of_four() {
        let err = encode_texture("x", &gradient(6, 4, 255), 6, 4, EncodeFormat::Bc1, None).unwrap_err();
        assert!(err.to_string().contains("multiples of 4"), "{err}");
        assert!(encode_texture("x", &gradient(6, 4, 255), 6, 4, EncodeFormat::Rgba8, None).is_ok());
    }
}
