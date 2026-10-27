//! DDS (DirectDraw Surface) reader: the inverse of [`YtdTexture::to_dds`].
//!
//! Reads the legacy FourCC / RGB-mask pixel formats and the DX10 extension
//! header, and maps each onto the [`TextureFormat`] the game stores. What
//! comes back is a [`YtdTexture`] with an empty name; the caller names it
//! (a `.ytd` builder uses the file stem).
use anyhow::{bail, Result};

use crate::resource::u32_le;
use crate::ytd::{TextureFormat, YtdTexture};

const DDS_MAGIC: &[u8; 4] = b"DDS ";
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x20000;

/// DXGI_FORMAT values the game's legacy formats correspond to.
fn format_from_dxgi(dxgi: u32) -> Option<TextureFormat> {
    Some(match dxgi {
        28 | 29 => TextureFormat::A8B8G8R8, // R8G8B8A8_UNORM(_SRGB)
        87 | 91 => TextureFormat::A8R8G8B8, // B8G8R8A8_UNORM(_SRGB)
        88 | 93 => TextureFormat::X8R8G8B8, // B8G8R8X8_UNORM(_SRGB)
        65      => TextureFormat::A8,       // A8_UNORM
        61      => TextureFormat::L8,       // R8_UNORM
        86      => TextureFormat::A1R5G5B5, // B5G5R5A1_UNORM
        71 | 72 => TextureFormat::DXT1,     // BC1_UNORM(_SRGB)
        74 | 75 => TextureFormat::DXT3,     // BC2_UNORM(_SRGB)
        77 | 78 => TextureFormat::DXT5,     // BC3_UNORM(_SRGB)
        80      => TextureFormat::ATI1,     // BC4_UNORM
        83      => TextureFormat::ATI2,     // BC5_UNORM
        98 | 99 => TextureFormat::BC7,      // BC7_UNORM(_SRGB)
        _ => return None,
    })
}

fn format_from_fourcc(fourcc: u32) -> Option<TextureFormat> {
    Some(match &fourcc.to_le_bytes() {
        b"DXT1" => TextureFormat::DXT1,
        b"DXT2" | b"DXT3" => TextureFormat::DXT3,
        b"DXT4" | b"DXT5" => TextureFormat::DXT5,
        b"ATI1" | b"BC4U" => TextureFormat::ATI1,
        b"ATI2" | b"BC5U" => TextureFormat::ATI2,
        _ => return None,
    })
}

fn format_from_masks(flags: u32, bits: u32, r: u32, g: u32, b: u32, a: u32) -> Option<TextureFormat> {
    let has_alpha = flags & DDPF_ALPHAPIXELS != 0;
    if flags & DDPF_RGB != 0 {
        return Some(match (bits, r, g, b) {
            (32, 0x00FF0000, 0x0000FF00, 0x000000FF) if has_alpha && a == 0xFF000000 => TextureFormat::A8R8G8B8,
            (32, 0x00FF0000, 0x0000FF00, 0x000000FF) => TextureFormat::X8R8G8B8,
            (32, 0x000000FF, 0x0000FF00, 0x00FF0000) if has_alpha && a == 0xFF000000 => TextureFormat::A8B8G8R8,
            (16, 0x7C00, 0x03E0, 0x001F) => TextureFormat::A1R5G5B5,
            _ => return None,
        });
    }
    if flags & DDPF_LUMINANCE != 0 && bits == 8 {
        return Some(TextureFormat::L8);
    }
    if flags & (DDPF_ALPHA | DDPF_ALPHAPIXELS) != 0 && bits == 8 && r == 0 && g == 0 && b == 0 {
        return Some(TextureFormat::A8);
    }
    None
}

/// Parses a DDS file into a nameless [`YtdTexture`]: the pixel data is kept
/// as the file holds it (every mip level, top first), `stride` and `levels`
/// are set the way the game stores them.
pub fn parse_dds(data: &[u8]) -> Result<YtdTexture> {
    if data.len() < 128 || &data[0..4] != DDS_MAGIC {
        bail!("not a DDS file (missing 'DDS ' magic)");
    }
    if u32_le(data, 4) != 124 {
        bail!("DDS header size is {}, expected 124", u32_le(data, 4));
    }
    let header_flags = u32_le(data, 8);
    let height = u32_le(data, 12);
    let width = u32_le(data, 16);
    let depth = if header_flags & 0x800000 != 0 { u32_le(data, 24).max(1) } else { 1 }; // DDSD_DEPTH
    let levels = if header_flags & 0x20000 != 0 { u32_le(data, 28).max(1) } else { 1 }; // DDSD_MIPMAPCOUNT

    // DDS_PIXELFORMAT at 76
    let pf_flags = u32_le(data, 80);
    let fourcc = u32_le(data, 84);
    let bits = u32_le(data, 88);
    let (rm, gm, bm, am) = (u32_le(data, 92), u32_le(data, 96), u32_le(data, 100), u32_le(data, 104));

    let mut body_start = 128;
    let format = if pf_flags & DDPF_FOURCC != 0 && &fourcc.to_le_bytes() == b"DX10" {
        if data.len() < 148 {
            bail!("DDS has a DX10 FourCC but no DX10 header");
        }
        let dxgi = u32_le(data, 128);
        let array_size = u32_le(data, 140);
        if array_size > 1 {
            bail!("DDS texture arrays ({array_size} layers) are not supported");
        }
        body_start = 148;
        format_from_dxgi(dxgi).ok_or_else(|| anyhow::anyhow!("DXGI format {dxgi} has no RAGE texture format"))?
    } else if pf_flags & DDPF_FOURCC != 0 {
        format_from_fourcc(fourcc).ok_or_else(|| {
            anyhow::anyhow!("DDS FourCC '{}' has no RAGE texture format", String::from_utf8_lossy(&fourcc.to_le_bytes()))
        })?
    } else {
        format_from_masks(pf_flags, bits, rm, gm, bm, am).ok_or_else(|| {
            anyhow::anyhow!(
                "DDS pixel format ({bits} bpp, masks R=0x{rm:08X} G=0x{gm:08X} B=0x{bm:08X} A=0x{am:08X}) has no RAGE texture format"
            )
        })?
    };

    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        bail!("DDS size {width}x{height} is out of range");
    }
    if depth > u16::MAX as u32 || levels > u8::MAX as u32 {
        bail!("DDS depth {depth} / mip count {levels} is out of range");
    }
    if format.is_block_compressed() && (width % 4 != 0 || height % 4 != 0) {
        bail!("{format} DDS is {width}x{height}; block-compressed sizes must be multiples of 4");
    }

    let (width, height, depth, levels) = (width as u16, height as u16, depth as u16, levels as u8);
    let stride = crate::ytd::stride_for(format, width, height);
    let expected = crate::ytd::mip_chain_size(format, width, height, levels) * depth as usize;
    let game_size = crate::ytd::ytd_chain_size(format, width, height, levels) * depth as usize;
    let body = &data[body_start..];
    // Whole blocks per level as DirectX lays a DDS out, or the game's own
    // sizes (levels below one block cut short) as some exports write them.
    let pixel_data = if body.len() >= expected {
        crate::ytd::to_ytd_layout(format, width, height, stride, levels, &body[..expected])
    } else if body.len() >= game_size {
        body[..game_size].to_vec()
    } else {
        bail!("DDS pixel data is {} bytes; {}x{} {} with {} mip(s) needs {}", body.len(), width, height, format, levels, expected);
    };

    Ok(YtdTexture {
        name: String::new(),
        name_hash: 0,
        width,
        height,
        depth,
        format,
        levels,
        stride,
        pixel_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(format: TextureFormat, width: u16, height: u16, levels: u8) -> YtdTexture {
        let size = crate::ytd::mip_chain_size(format, width, height, levels);
        YtdTexture {
            name: "x".into(),
            name_hash: 1,
            width,
            height,
            depth: 1,
            format,
            levels,
            stride: crate::ytd::stride_for(format, width, height),
            pixel_data: (0..size).map(|i| (i * 7 % 251) as u8).collect(),
        }
    }

    #[test]
    fn round_trips_every_format_through_to_dds() {
        for format in [
            TextureFormat::DXT1, TextureFormat::DXT3, TextureFormat::DXT5, TextureFormat::ATI1, TextureFormat::ATI2,
            TextureFormat::BC7, TextureFormat::A8R8G8B8, TextureFormat::X8R8G8B8, TextureFormat::A8B8G8R8,
            TextureFormat::A1R5G5B5, TextureFormat::A8, TextureFormat::L8,
        ] {
            let tex = sample(format, 16, 8, 2);
            let back = parse_dds(&tex.to_dds()).unwrap_or_else(|e| panic!("{format}: {e}"));
            assert_eq!(back.format, format, "{format}");
            assert_eq!((back.width, back.height, back.levels, back.stride), (16, 8, 2, tex.stride), "{format}");
            assert_eq!(back.pixel_data, tex.pixel_data, "{format}");
        }
    }

    #[test]
    fn rejects_non_dds_and_short_data() {
        assert!(parse_dds(b"PNG\r\n").is_err());
        let mut dds = sample(TextureFormat::DXT5, 8, 8, 1).to_dds();
        dds.truncate(128 + 10);
        let err = parse_dds(&dds).unwrap_err().to_string();
        assert!(err.contains("pixel data is 10 bytes"), "{err}");
        assert!(err.contains("needs 64"), "{err}");
    }

    #[test]
    fn rejects_block_sizes_that_are_not_multiples_of_four() {
        let mut dds = sample(TextureFormat::DXT1, 8, 8, 1).to_dds();
        dds[16..20].copy_from_slice(&6u32.to_le_bytes());
        let err = parse_dds(&dds).unwrap_err().to_string();
        assert!(err.contains("multiples of 4"), "{err}");
    }
}
