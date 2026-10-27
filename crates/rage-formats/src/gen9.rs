//! GTA V Enhanced ("Gen9") resources.
//!
//! The Enhanced edition keeps the RSC7 container (same header, same page flags, same deflate) but
//! stores `.ytd`, `.ydr`, `.ydd`, `.yft` and `.ypt` under other resource versions and with other
//! struct layouts. Everything else (`.ybn`, meta, `.ymap`, ...) is byte-identical between the two.
//! This module tells the editions apart and holds what the Gen9 → Legacy conversion shares: the
//! texture format table first, vertex and shader tables as their readers arrive.

use anyhow::{bail, Result};

use crate::resource::{resource_version_from_flags, RSC7_MAGIC, RSC8_MAGIC};
use crate::ytd::TextureFormat;

/// Which edition of the game a resource file is laid out for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// The PC edition FiveM runs (build 3751 and earlier).
    Legacy,
    /// GTA V Enhanced.
    Gen9,
}

impl std::fmt::Display for Edition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Legacy => "Legacy", Self::Gen9 => "Enhanced (Gen9)" })
    }
}

impl Edition {
    /// The edition a resource of extension `ext` (no dot, any case) and RSC7 `version` belongs to.
    /// `None` for a type whose layout does not depend on the edition and for a version that is neither.
    ///
    /// | ext | Legacy | Gen9 |
    /// |---|---|---|
    /// | `ytd` | 13 | 5 |
    /// | `ydr`, `ydd` | 165 | 159 (the reference tool also reads 154 as Gen9) |
    /// | `yft` | 162 | 171 |
    /// | `ypt` | 68 | 71 |
    pub fn of(ext: &str, version: u32) -> Option<Self> {
        let (legacy, gen9): (&[u32], &[u32]) = match ext.to_ascii_lowercase().as_str() {
            "ytd" => (&[13], &[5]),
            "ydr" | "ydd" => (&[165], &[159, 154]),
            "yft" => (&[162], &[171]),
            "ypt" => (&[68], &[71]),
            _ => return None,
        };
        if legacy.contains(&version) { Some(Self::Legacy) } else if gen9.contains(&version) { Some(Self::Gen9) } else { None }
    }

    /// [`Edition::of`] for a whole file: reads the version out of its RSC7 header. An `RSC8` header
    /// (a different game altogether, RDR2) is an error rather than `None`.
    pub fn of_file(ext: &str, data: &[u8]) -> Result<Option<Self>> {
        if data.len() < 16 { bail!("resource data too short ({} bytes)", data.len()); }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic == RSC8_MAGIC { bail!("RSC8 header: not a GTA V resource"); }
        if magic != RSC7_MAGIC { bail!("not an RSC7 file (magic = 0x{magic:08X})"); }
        let sys = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let gfx = u32::from_le_bytes(data[12..16].try_into().unwrap());
        Ok(Self::of(ext, resource_version_from_flags(sys, gfx)))
    }
}

/// `rage::sga::BufferFormat`, the DXGI-like byte a Gen9 texture or vertex element carries: its name for
/// the values this crate knows, for messages.
pub fn format_name(v: u8) -> String {
    let n = match v {
        0x00 => "UNKNOWN", 0x02 => "R32G32B32A32_FLOAT", 0x06 => "R32G32B32_FLOAT", 0x0A => "R16G16B16A16_FLOAT",
        0x0B => "R16G16B16A16_UNORM", 0x10 => "R32G32_FLOAT", 0x18 => "R10G10B10A2_UNORM", 0x1A => "R11G11B10_FLOAT",
        0x1C => "R8G8B8A8_UNORM", 0x1D => "R8G8B8A8_UNORM_SRGB", 0x22 => "R16G16_FLOAT", 0x23 => "R16G16_UNORM",
        0x29 => "R32_FLOAT", 0x31 => "R8G8_UNORM", 0x36 => "R16_FLOAT", 0x38 => "R16_UNORM", 0x3D => "R8_UNORM",
        0x41 => "A8_UNORM", 0x47 => "BC1_UNORM", 0x48 => "BC1_UNORM_SRGB", 0x4A => "BC2_UNORM", 0x4B => "BC2_UNORM_SRGB",
        0x4D => "BC3_UNORM", 0x4E => "BC3_UNORM_SRGB", 0x50 => "BC4_UNORM", 0x51 => "BC4_SNORM", 0x53 => "BC5_UNORM",
        0x54 => "BC5_SNORM", 0x55 => "B5G6R5_UNORM", 0x56 => "B5G5R5A1_UNORM", 0x57 => "B8G8R8A8_UNORM",
        0x5B => "B8G8R8A8_UNORM_SRGB", 0x5F => "BC6H_UF16", 0x60 => "BC6H_SF16", 0x62 => "BC7_UNORM",
        0x63 => "BC7_UNORM_SRGB", 0x73 => "B4G4R4A4_UNORM",
        _ => return format!("0x{v:02X}"),
    };
    format!("{n} (0x{v:02X})")
}

/// The legacy texture format holding the same bytes as a Gen9 one.
///
/// The legacy game has no sRGB variants (the shader decides how to read a texture), so the sRGB twins of a
/// format map to the same legacy format as the plain one: the pixel bytes are identical, only the colour
/// space flag is dropped. Formats the legacy game cannot hold at all (16-bit and float formats, BC6H,
/// `R8G8`, 10-bit packed, ...) are an error, never a lookalike with the wrong size.
pub fn legacy_texture_format(gen9: u8) -> Result<TextureFormat> {
    Ok(match gen9 {
        0x1C | 0x1D => TextureFormat::A8B8G8R8,
        0x57 | 0x5B => TextureFormat::A8R8G8B8,
        0x41 => TextureFormat::A8,
        0x3D => TextureFormat::L8,
        0x56 => TextureFormat::A1R5G5B5,
        0x47 | 0x48 => TextureFormat::DXT1,
        0x4A | 0x4B => TextureFormat::DXT3,
        0x4D | 0x4E => TextureFormat::DXT5,
        0x50 => TextureFormat::ATI1,
        0x53 => TextureFormat::ATI2,
        0x62 | 0x63 => TextureFormat::BC7,
        v => bail!("texture format {} has no legacy equivalent", format_name(v)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editions_follow_the_resource_version() {
        assert_eq!(Edition::of("ytd", 13), Some(Edition::Legacy));
        assert_eq!(Edition::of("YTD", 5), Some(Edition::Gen9));
        assert_eq!(Edition::of("ydr", 165), Some(Edition::Legacy));
        assert_eq!(Edition::of("ydd", 159), Some(Edition::Gen9));
        assert_eq!(Edition::of("ydr", 154), Some(Edition::Gen9));
        assert_eq!(Edition::of("yft", 162), Some(Edition::Legacy));
        assert_eq!(Edition::of("yft", 171), Some(Edition::Gen9));
        assert_eq!(Edition::of("ypt", 71), Some(Edition::Gen9));
        assert_eq!(Edition::of("ytd", 99), None);
        assert_eq!(Edition::of("ybn", 43), None);
    }

    #[test]
    fn the_edition_comes_from_the_header_flags() {
        // the real s95.ytd header: version 5 -> system nibble 0, graphics nibble 5
        let mut h = Vec::new();
        for v in [RSC7_MAGIC, 13u32, 0x0002_0000, 0x5004_0090] { h.extend_from_slice(&v.to_le_bytes()); }
        assert_eq!(Edition::of_file("ytd", &h).unwrap(), Some(Edition::Gen9));
        // and a legacy one: version 13 -> 0 and 13 split as 0x0 / 0xD
        let mut h = Vec::new();
        for v in [RSC7_MAGIC, 13u32, 0x0002_0000, 0xD004_0090] { h.extend_from_slice(&v.to_le_bytes()); }
        assert_eq!(Edition::of_file("ytd", &h).unwrap(), Some(Edition::Legacy));
        assert!(Edition::of_file("ytd", &[0; 16]).is_err());
        assert!(Edition::of_file("ytd", &[0; 3]).is_err());
    }

    #[test]
    fn formats_map_by_bytes_and_refuse_the_rest() {
        assert_eq!(legacy_texture_format(0x47).unwrap(), TextureFormat::DXT1);
        assert_eq!(legacy_texture_format(0x4E).unwrap(), TextureFormat::DXT5);
        assert_eq!(legacy_texture_format(0x63).unwrap(), TextureFormat::BC7);
        assert_eq!(legacy_texture_format(0x1D).unwrap(), TextureFormat::A8B8G8R8);
        let e = legacy_texture_format(0x5F).unwrap_err().to_string();
        assert!(e.contains("BC6H_UF16") && e.contains("no legacy equivalent"), "{e}");
        assert!(legacy_texture_format(0x38).is_err());
    }
}
