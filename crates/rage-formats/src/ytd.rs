/// YTD (Texture Dictionary) parser for GTA V (Gen8 / PC format).
///
/// Accepts the standalone RSC7 bytes as returned by `RpfArchive::extract_entry`.
use anyhow::{Result, Context};
use crate::resource::{ResReader, prepare_rsc7, u16_le, u32_le, u64_le, SYSTEM_BASE};

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TextureFormat {
    A8R8G8B8 = 21,
    X8R8G8B8 = 22,
    A1R5G5B5 = 25,
    A8       = 28,
    A8B8G8R8 = 32,
    L8       = 50,
    DXT1     = 0x31545844,
    DXT3     = 0x33545844,
    DXT5     = 0x35545844,
    ATI1     = 0x31495441,
    ATI2     = 0x32495441,
    BC7      = 0x20374342,
    Unknown  = 0,
}

impl TextureFormat {
    pub fn from_u32(v: u32) -> Self {
        match v {
            21          => Self::A8R8G8B8,
            22          => Self::X8R8G8B8,
            25          => Self::A1R5G5B5,
            28          => Self::A8,
            32          => Self::A8B8G8R8,
            50          => Self::L8,
            0x31545844  => Self::DXT1,
            0x33545844  => Self::DXT3,
            0x35545844  => Self::DXT5,
            0x31495441  => Self::ATI1,
            0x32495441  => Self::ATI2,
            0x20374342  => Self::BC7,
            _           => Self::Unknown,
        }
    }

    pub fn is_block_compressed(self) -> bool {
        matches!(self, Self::DXT1 | Self::DXT3 | Self::DXT5 | Self::ATI1 | Self::ATI2 | Self::BC7)
    }

    /// The name the reference tool's `TextureFormat` enum gives it (`D3DFMT_DXT1`), as its XML spells it.
    pub fn reference_name(self) -> &'static str {
        match self {
            Self::A8R8G8B8 => "D3DFMT_A8R8G8B8",
            Self::X8R8G8B8 => "D3DFMT_X8R8G8B8",
            Self::A1R5G5B5 => "D3DFMT_A1R5G5B5",
            Self::A8       => "D3DFMT_A8",
            Self::A8B8G8R8 => "D3DFMT_A8B8G8R8",
            Self::L8       => "D3DFMT_L8",
            Self::DXT1     => "D3DFMT_DXT1",
            Self::DXT3     => "D3DFMT_DXT3",
            Self::DXT5     => "D3DFMT_DXT5",
            Self::ATI1     => "D3DFMT_ATI1",
            Self::ATI2     => "D3DFMT_ATI2",
            Self::BC7      => "D3DFMT_BC7",
            Self::Unknown  => "D3DFMT_UNKNOWN",
        }
    }

    /// The inverse of [`TextureFormat::reference_name`] (case-insensitive); `None` for any other text.
    pub fn from_reference_name(s: &str) -> Option<Self> {
        const ALL: [TextureFormat; 12] = [
            TextureFormat::A8R8G8B8, TextureFormat::X8R8G8B8, TextureFormat::A1R5G5B5, TextureFormat::A8,
            TextureFormat::A8B8G8R8, TextureFormat::L8, TextureFormat::DXT1, TextureFormat::DXT3,
            TextureFormat::DXT5, TextureFormat::ATI1, TextureFormat::ATI2, TextureFormat::BC7,
        ];
        let s = s.trim();
        ALL.into_iter().find(|f| f.reference_name().eq_ignore_ascii_case(s))
    }
}

impl std::fmt::Display for TextureFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::A8R8G8B8 => "A8R8G8B8",
            Self::X8R8G8B8 => "X8R8G8B8",
            Self::A1R5G5B5 => "A1R5G5B5",
            Self::A8       => "A8",
            Self::A8B8G8R8 => "A8B8G8R8",
            Self::L8       => "L8",
            Self::DXT1     => "DXT1",
            Self::DXT3     => "DXT3",
            Self::DXT5     => "DXT5",
            Self::ATI1     => "ATI1",
            Self::ATI2     => "ATI2",
            Self::BC7      => "BC7",
            Self::Unknown  => "Unknown",
        };
        f.write_str(s)
    }
}

/// One texture entry extracted from a YTD. `pixel_data` holds every mip
/// level as the game stores them (see [`ytd_chain_size`]); [`YtdTexture::to_dds`]
/// re-lays it out the way a DDS file expects.
#[derive(Debug, Clone)]
pub struct YtdTexture {
    pub name: String,
    pub name_hash: u32,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub format: TextureFormat,
    pub levels: u8,
    pub stride: u16,
    pub pixel_data: Vec<u8>,
}

impl YtdTexture {
    /// Serialize this texture to a DDS file.
    pub fn to_dds(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // DDS magic
        out.extend_from_slice(b"DDS ");

        // DDS_HEADER (124 bytes)
        let has_mips = self.levels > 1;
        let is_compressed = self.format.is_block_compressed();

        let mut flags: u32 = 0x1 | 0x2 | 0x4 | 0x1000; // CAPS | HEIGHT | WIDTH | PIXELFORMAT
        if has_mips { flags |= 0x20000; } // MIPMAPCOUNT
        if is_compressed { flags |= 0x80000; } else { flags |= 0x8; } // LINEARSIZE or PITCH

        let pitch_or_linear: u32 = self.stride as u32 * self.height as u32;

        out.extend_from_slice(&124u32.to_le_bytes());            // dwSize
        out.extend_from_slice(&flags.to_le_bytes());             // dwFlags
        out.extend_from_slice(&(self.height as u32).to_le_bytes()); // dwHeight
        out.extend_from_slice(&(self.width as u32).to_le_bytes());  // dwWidth
        out.extend_from_slice(&pitch_or_linear.to_le_bytes());   // dwPitchOrLinearSize
        out.extend_from_slice(&(self.depth as u32).to_le_bytes()); // dwDepth
        out.extend_from_slice(&(self.levels as u32).to_le_bytes()); // dwMipMapCount
        out.extend_from_slice(&[0u8; 44]);                       // dwReserved1[11]

        // DDS_PIXELFORMAT (32 bytes)
        self.write_pixelformat(&mut out);

        let mut caps: u32 = 0x1000; // DDSCAPS_TEXTURE
        if has_mips { caps |= 0x8 | 0x400000; } // COMPLEX | MIPMAP
        out.extend_from_slice(&caps.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]); // Caps2/3/4 + Reserved2

        // Pixel data
        if self.format == TextureFormat::BC7 {
            write_dx10_header(&mut out);
        }
        out.extend_from_slice(&to_dds_layout(self.format, self.width, self.height, self.stride, self.levels, &self.pixel_data));

        out
    }

    fn write_pixelformat(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&32u32.to_le_bytes()); // dwSize
        match self.format {
            TextureFormat::DXT1 | TextureFormat::DXT3 | TextureFormat::DXT5
            | TextureFormat::ATI1 | TextureFormat::ATI2 => {
                out.extend_from_slice(&0x4u32.to_le_bytes()); // DDPF_FOURCC
                out.extend_from_slice(&(self.format as u32).to_le_bytes()); // FourCC
                out.extend_from_slice(&[0u8; 20]); // RGB counts + masks
            }
            TextureFormat::BC7 => {
                out.extend_from_slice(&0x4u32.to_le_bytes()); // DDPF_FOURCC
                out.extend_from_slice(b"DX10");               // FourCC = DX10
                out.extend_from_slice(&[0u8; 20]);
            }
            // Uncompressed: (flags, bit count, R, G, B, A masks)
            TextureFormat::A8R8G8B8 => write_masks(out, 0x41, 32, 0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000),
            TextureFormat::X8R8G8B8 => write_masks(out, 0x40, 32, 0x00FF0000, 0x0000FF00, 0x000000FF, 0),
            TextureFormat::A8B8G8R8 => write_masks(out, 0x41, 32, 0x000000FF, 0x0000FF00, 0x00FF0000, 0xFF000000),
            TextureFormat::A1R5G5B5 => write_masks(out, 0x41, 16, 0x7C00, 0x03E0, 0x001F, 0x8000),
            TextureFormat::A8       => write_masks(out, 0x2, 8, 0, 0, 0, 0xFF),
            TextureFormat::L8       => write_masks(out, 0x20000, 8, 0xFF, 0, 0, 0),
            TextureFormat::Unknown  => out.extend_from_slice(&[0u8; 28]),
        }
    }
}

fn write_masks(out: &mut Vec<u8>, flags: u32, bits: u32, r: u32, g: u32, b: u32, a: u32) {
    for v in [flags, 0, bits, r, g, b, a] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Bytes per 4x4 block for a compressed format, or bits per pixel otherwise.
fn format_density(format: TextureFormat) -> (bool, usize) {
    match format {
        TextureFormat::DXT1 | TextureFormat::ATI1 => (true, 8),
        TextureFormat::DXT3 | TextureFormat::DXT5 | TextureFormat::ATI2 | TextureFormat::BC7 => (true, 16),
        TextureFormat::A8R8G8B8 | TextureFormat::X8R8G8B8 | TextureFormat::A8B8G8R8 => (false, 32),
        TextureFormat::A1R5G5B5 => (false, 16),
        TextureFormat::A8 | TextureFormat::L8 => (false, 8),
        TextureFormat::Unknown => (false, 0),
    }
}

/// Byte size of one mip level as laid out in memory (DirectXTex's slice
/// pitch: whole 4x4 blocks for compressed formats).
pub fn level_size(format: TextureFormat, width: u16, height: u16) -> usize {
    let (blocks, density) = format_density(format);
    if blocks {
        let bw = (width as usize).div_ceil(4).max(1);
        let bh = (height as usize).div_ceil(4).max(1);
        bw * bh * density
    } else {
        (width as usize * density).div_ceil(8) * height as usize
    }
}

/// The `stride` the game stores for a texture: the top level's slice pitch
/// divided by its height, which is what the reference tool writes on import too.
pub fn stride_for(format: TextureFormat, width: u16, height: u16) -> u16 {
    (level_size(format, width, height) / height.max(1) as usize) as u16
}

/// Total bytes of a `levels`-deep mip chain, each level half the previous
/// in both directions (never below 1 pixel).
pub fn mip_chain_size(format: TextureFormat, width: u16, height: u16, levels: u8) -> usize {
    let (mut w, mut h) = (width, height);
    let mut total = 0;
    for _ in 0..levels {
        total += level_size(format, w, h);
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    total
}

/// Byte size of each level in the two layouts a mip chain has: how the
/// game stores it (each level a quarter of the previous, counting down
/// from `stride * height`, so a 2x2 or 1x1 block level is 4 or 1 bytes) and
/// how a DDS stores it (whole 4x4 blocks, or whole rows).
fn level_sizes(format: TextureFormat, width: u16, height: u16, stride: u16, levels: u8) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(levels as usize);
    let (mut w, mut h) = (width, height);
    let mut game = stride as usize * height as usize;
    for _ in 0..levels {
        out.push((game, level_size(format, w, h)));
        game /= 4;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    out
}

/// Total bytes of a mip chain as the game stores it in a `.ytd` (see
/// [`level_sizes`]); equals [`mip_chain_size`] while every level is at
/// least a whole block.
pub fn ytd_chain_size(format: TextureFormat, width: u16, height: u16, levels: u8) -> usize {
    calc_pixel_data_size(stride_for(format, width, height), height, levels)
}

/// Re-lays a chain stored the game's way out as a DDS holds it, padding
/// each level below one block with zeros.
pub fn to_dds_layout(format: TextureFormat, width: u16, height: u16, stride: u16, levels: u8, data: &[u8]) -> Vec<u8> {
    let sizes = level_sizes(format, width, height, stride, levels);
    let mut out = Vec::with_capacity(sizes.iter().map(|s| s.1).sum());
    let mut at = 0;
    for (game, dds) in sizes {
        let have = data.get(at..(at + game).min(data.len())).unwrap_or(&[]);
        let n = have.len().min(dds);
        out.extend_from_slice(&have[..n]);
        out.resize(out.len() + dds - n, 0);
        at += game;
    }
    out
}

/// The inverse of [`to_dds_layout`]: a DDS chain re-laid out as the game
/// stores it (a level below one block keeps only its first bytes).
pub fn to_ytd_layout(format: TextureFormat, width: u16, height: u16, stride: u16, levels: u8, data: &[u8]) -> Vec<u8> {
    let sizes = level_sizes(format, width, height, stride, levels);
    let mut out = Vec::with_capacity(sizes.iter().map(|s| s.0).sum());
    let mut at = 0;
    for (game, dds) in sizes {
        let have = data.get(at..(at + dds).min(data.len())).unwrap_or(&[]);
        let n = have.len().min(game);
        out.extend_from_slice(&have[..n]);
        out.resize(out.len() + game - n, 0);
        at += dds;
    }
    out
}

/// How many mip levels a texture of this size can carry so that the game's
/// own size arithmetic (each level a quarter of the previous) stays exact:
/// down to 4x4 for block-compressed formats, 1x1 otherwise.
pub fn full_mip_count(format: TextureFormat, width: u16, height: u16) -> u8 {
    let floor = if format.is_block_compressed() { 4 } else { 1 };
    let mut levels = 1u8;
    let (mut w, mut h) = (width, height);
    while w / 2 >= floor && h / 2 >= floor && levels < u8::MAX {
        w /= 2;
        h /= 2;
        levels += 1;
    }
    levels
}

fn write_dx10_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&98u32.to_le_bytes()); // DXGI_FORMAT_BC7_UNORM
    out.extend_from_slice(&3u32.to_le_bytes());  // D3D10_RESOURCE_DIMENSION_TEXTURE2D
    out.extend_from_slice(&0u32.to_le_bytes());  // miscFlag
    out.extend_from_slice(&1u32.to_le_bytes());  // arraySize
    out.extend_from_slice(&0u32.to_le_bytes());  // miscFlags2
}

// ─── Parser ───────────────────────────────────────────────────────────────────

pub fn parse_ytd(data: &[u8]) -> Result<Vec<YtdTexture>> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    parse_texture_dict_at(&reader, SYSTEM_BASE)
}

/// The textures of a dictionary from its system section alone (see
/// [`crate::prepare_rsc7_system`]): names, sizes, formats and mip counts,
/// with every `pixel_data` left empty.
pub fn parse_ytd_system(system: &[u8]) -> Result<Vec<YtdTexture>> {
    let reader = ResReader { system, graphics: &[] };
    parse_texture_dict_with(&reader, SYSTEM_BASE, false)
}

pub(crate) fn parse_texture_dict_at(reader: &ResReader<'_>, va: u64) -> Result<Vec<YtdTexture>> {
    parse_texture_dict_with(reader, va, true)
}

fn parse_texture_dict_with(reader: &ResReader<'_>, va: u64, with_pixels: bool) -> Result<Vec<YtdTexture>> {
    let dict = reader.resolve(va, 0x40)
        .ok_or_else(|| anyhow::anyhow!("system section too small for TextureDictionary"))?;

    let hash_ptr   = u64_le(dict, 0x20);
    let hash_count = u16_le(dict, 0x28) as usize;
    let tex_ptr_array = u64_le(dict, 0x30);
    let tex_count     = u16_le(dict, 0x38) as usize;

    let hash_data = if hash_count > 0 {
        reader.resolve(hash_ptr, hash_count * 4)
    } else {
        None
    };

    let ptr_data = if tex_count > 0 {
        reader.resolve(tex_ptr_array, tex_count * 8)
            .with_context(|| format!("texture pointer array out of bounds (va=0x{:X})", tex_ptr_array))?
    } else {
        return Ok(vec![]);
    };

    let mut textures = Vec::with_capacity(tex_count);
    for i in 0..tex_count {
        let tex_va = u64_le(ptr_data, i * 8);
        if tex_va == 0 { continue; }

        let name_hash = hash_data
            .and_then(|h| h.get(i * 4..i * 4 + 4))
            .map(|b| u32_le(b, 0))
            .unwrap_or(0);

        match parse_texture(tex_va, name_hash, reader, with_pixels) {
            Ok(tex) => textures.push(tex),
            Err(e) => eprintln!("[YTD] Warning: texture {} parse error: {}", i, e),
        }
    }

    Ok(textures)
}

fn parse_texture(tex_va: u64, name_hash: u32, reader: &ResReader<'_>, with_pixels: bool) -> Result<YtdTexture> {
    let raw = reader.resolve(tex_va, 0x90)
        .with_context(|| format!("texture struct out of bounds (va=0x{:X})", tex_va))?;

    let name_ptr = u64_le(raw, 0x28);
    let width  = u16_le(raw, 0x50);
    let height = u16_le(raw, 0x52);
    let depth  = u16_le(raw, 0x54);
    let stride = u16_le(raw, 0x56);
    let fmt    = TextureFormat::from_u32(u32_le(raw, 0x58));
    let levels = raw[0x5D];
    let data_ptr = u64_le(raw, 0x70);

    let name = reader.string_at(name_ptr).unwrap_or_default();
    let pixel_size = calc_pixel_data_size(stride, height, levels);

    let pixel_data = if with_pixels && pixel_size > 0 && data_ptr != 0 {
        reader.resolve(data_ptr, pixel_size)
            .with_context(|| format!("pixel data out of bounds (va=0x{:X}, size={})", data_ptr, pixel_size))?
            .to_vec()
    } else {
        vec![]
    };

    Ok(YtdTexture { name, name_hash, width, height, depth, format: fmt, levels, stride, pixel_data })
}

fn calc_pixel_data_size(stride: u16, height: u16, levels: u8) -> usize {
    let mut total = 0usize;
    let mut length = stride as usize * height as usize;
    for _ in 0..levels {
        total += length;
        length /= 4;
    }
    total
}

// ─── Writer ───────────────────────────────────────────────────────────────────

/// Resource version of a .ytd.
pub const YTD_VERSION: u32 = 13;

const DICT_SIZE: usize = 0x40;
const TEXTURE_SIZE: usize = 0x90;

/// Builds a .ytd (RSC7, version 13) from textures, laid out as the reference tool's
/// `TextureDictionary.Save` does: entries sorted by name hash, the hash and
/// pointer lists, texture structs and name strings packed into system pages,
/// each texture's pixel data packed into graphics pages.
///
/// A texture whose `name_hash` is 0 is hashed from its lowercased name.
/// Duplicate hashes are an error, since a dictionary is looked up by hash.
pub fn serialize_ytd(textures: &[YtdTexture]) -> Result<Vec<u8>> {
    use crate::resource::{build_rsc7_with_flags, pack_pages, rsc7_page_count, GRAPHICS_BASE};

    let mut entries: Vec<(u32, &YtdTexture)> = textures
        .iter()
        .map(|t| {
            let hash = if t.name_hash != 0 { t.name_hash } else { crate::rage_joaat(&t.name.to_lowercase()) };
            (hash, t)
        })
        .collect();
    entries.sort_by_key(|(hash, _)| *hash);
    if let Some(w) = entries.windows(2).find(|w| w[0].0 == w[1].0) {
        anyhow::bail!("textures '{}' and '{}' share the name hash 0x{:08X}", w[0].1.name, w[1].1.name, w[0].0);
    }
    let count = entries.len();
    if count > u16::MAX as usize {
        anyhow::bail!("{count} textures; a dictionary holds at most 65535");
    }
    for (_, tex) in &entries {
        let expected = calc_pixel_data_size(tex.stride, tex.height, tex.levels) * tex.depth.max(1) as usize;
        if tex.pixel_data.len() < expected {
            anyhow::bail!(
                "texture '{}': {}x{} {} with {} mip(s) needs {} bytes of pixel data, has {}",
                tex.name, tex.width, tex.height, tex.format, tex.levels, expected, tex.pixel_data.len()
            );
        }
    }

    // Graphics: one block per texture with pixel data.
    let data_len = |t: &YtdTexture| calc_pixel_data_size(t.stride, t.height, t.levels) * t.depth.max(1) as usize;
    let gfx_sizes: Vec<usize> = entries.iter().map(|(_, t)| data_len(t)).filter(|&n| n > 0).collect();
    let gfx = pack_pages(&gfx_sizes, false, 128)?;
    let gfx_pages = rsc7_page_count(gfx.flags);

    // System: dictionary, page map, hash list, pointer list, textures, names.
    // The page map's size depends on the page count, so lay out until stable.
    let mut sys_pages = 1;
    let sys = loop {
        let pagemap_size = 16 + 8 * (sys_pages + gfx_pages);
        let mut sizes = vec![DICT_SIZE, pagemap_size, count * 4, count * 8];
        sizes.extend(std::iter::repeat_n(TEXTURE_SIZE, count));
        sizes.extend(entries.iter().map(|(_, t)| t.name.len() + 1));
        let layout = pack_pages(&sizes, true, 128 - gfx_pages)?;
        let pages = rsc7_page_count(layout.flags);
        if pages == sys_pages {
            break layout;
        }
        sys_pages = pages;
    };

    let mut system = vec![0u8; sys.size];
    let va = |off: usize| SYSTEM_BASE + off as u64;
    let put_u16 = |buf: &mut [u8], off: usize, v: u16| buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    let put_u32 = |buf: &mut [u8], off: usize, v: u32| buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put_u64 = |buf: &mut [u8], off: usize, v: u64| buf[off..off + 8].copy_from_slice(&v.to_le_bytes());

    let [dict_off, pagemap_off, hashes_off, ptrs_off] = [sys.offsets[0], sys.offsets[1], sys.offsets[2], sys.offsets[3]];
    let tex_offs = &sys.offsets[4..4 + count];
    let name_offs = &sys.offsets[4 + count..];

    put_u32(&mut system, dict_off + 0x04, 1);
    put_u64(&mut system, dict_off + 0x08, va(pagemap_off));
    put_u32(&mut system, dict_off + 0x18, 1);
    put_u64(&mut system, dict_off + 0x20, if count > 0 { va(hashes_off) } else { 0 });
    put_u16(&mut system, dict_off + 0x28, count as u16);
    put_u16(&mut system, dict_off + 0x2A, count as u16);
    put_u64(&mut system, dict_off + 0x30, if count > 0 { va(ptrs_off) } else { 0 });
    put_u16(&mut system, dict_off + 0x38, count as u16);
    put_u16(&mut system, dict_off + 0x3A, count as u16);

    system[pagemap_off + 8] = sys_pages as u8;
    system[pagemap_off + 9] = gfx_pages as u8;

    let mut graphics = vec![0u8; gfx.size];
    let mut gfx_block = 0;
    for (i, (hash, tex)) in entries.iter().enumerate() {
        put_u32(&mut system, hashes_off + i * 4, *hash);
        put_u64(&mut system, ptrs_off + i * 8, va(tex_offs[i]));

        let t = tex_offs[i];
        put_u32(&mut system, t + 0x04, 1);
        put_u64(&mut system, t + 0x28, va(name_offs[i]));
        put_u16(&mut system, t + 0x30, 1);
        put_u16(&mut system, t + 0x50, tex.width);
        put_u16(&mut system, t + 0x52, tex.height);
        put_u16(&mut system, t + 0x54, tex.depth.max(1));
        put_u16(&mut system, t + 0x56, tex.stride);
        put_u32(&mut system, t + 0x58, tex.format as u32);
        system[t + 0x5D] = tex.levels;
        let len = data_len(tex);
        if len > 0 {
            let off = gfx.offsets[gfx_block];
            gfx_block += 1;
            graphics[off..off + len].copy_from_slice(&tex.pixel_data[..len]);
            put_u64(&mut system, t + 0x70, GRAPHICS_BASE + off as u64);
        }

        let n = name_offs[i];
        system[n..n + tex.name.len()].copy_from_slice(tex.name.as_bytes());
    }

    Ok(build_rsc7_with_flags(YTD_VERSION, sys.flags, &system, gfx.flags, &graphics))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 4x4 DXT1 texture with one mip level and one block of fixed pixel data.
    pub(crate) fn sample_dxt1_4x4(name: &str) -> YtdTexture {
        YtdTexture {
            name: name.into(),
            name_hash: crate::rage_joaat(name),
            width: 4,
            height: 4,
            depth: 1,
            format: TextureFormat::DXT1,
            levels: 1,
            stride: stride_for(TextureFormat::DXT1, 4, 4),
            pixel_data: vec![0x1F, 0x00, 0xE0, 0x07, 0x1B, 0x1B, 0x1B, 0x1B],
        }
    }

    fn tex(name: &str, format: TextureFormat, width: u16, height: u16) -> YtdTexture {
        let levels = full_mip_count(format, width, height);
        let size = mip_chain_size(format, width, height, levels);
        YtdTexture {
            name: name.into(),
            name_hash: 0,
            width,
            height,
            depth: 1,
            format,
            levels,
            stride: stride_for(format, width, height),
            pixel_data: (0..size).map(|i| (i.wrapping_mul(31) % 253) as u8).collect(),
        }
    }

    #[test]
    fn mip_arithmetic_matches_the_games() {
        assert_eq!(stride_for(TextureFormat::DXT5, 512, 512), 512);
        assert_eq!(stride_for(TextureFormat::DXT1, 512, 256), 256);
        assert_eq!(stride_for(TextureFormat::A8R8G8B8, 64, 64), 256);
        assert_eq!(full_mip_count(TextureFormat::DXT5, 512, 256), 7);
        assert_eq!(full_mip_count(TextureFormat::DXT1, 4, 4), 1);
        assert_eq!(full_mip_count(TextureFormat::A8R8G8B8, 8, 2), 2);
        // The parser's size formula (stride*height, quartered per level) and
        // the true chain size agree for every level the count allows.
        let t = tex("a", TextureFormat::DXT5, 512, 256);
        assert_eq!(calc_pixel_data_size(t.stride, t.height, t.levels), t.pixel_data.len());
    }

    #[test]
    fn serialize_round_trips_through_parse_ytd() {
        let input = vec![
            tex("Zed_diffuse", TextureFormat::DXT1, 256, 128),
            tex("alpha_n", TextureFormat::ATI2, 64, 64),
            tex("big", TextureFormat::BC7, 1024, 1024),
            tex("flat", TextureFormat::A8R8G8B8, 16, 16),
        ];
        let bytes = serialize_ytd(&input).unwrap();
        let back = parse_ytd(&bytes).unwrap();
        assert_eq!(back.len(), input.len());
        // Sorted by hash on the way out.
        let mut hashes: Vec<u32> = back.iter().map(|t| t.name_hash).collect();
        hashes.sort();
        assert_eq!(hashes, back.iter().map(|t| t.name_hash).collect::<Vec<_>>());
        for want in &input {
            let got = back.iter().find(|t| t.name == want.name).unwrap_or_else(|| panic!("{} missing", want.name));
            assert_eq!(got.name_hash, crate::rage_joaat(&want.name.to_lowercase()));
            assert_eq!((got.width, got.height, got.depth, got.format, got.levels, got.stride),
                       (want.width, want.height, 1, want.format, want.levels, want.stride), "{}", want.name);
            assert_eq!(got.pixel_data, want.pixel_data, "{}", want.name);
        }
        assert_eq!(crate::resource::u32_le(&bytes, 4), YTD_VERSION);
    }


    #[test]
    fn tail_mips_below_one_block_keep_the_games_sizes_and_pad_out_to_dds() {
        // 256x256 DXT5 down to 1x1: the game stores 9 levels in 87381 bytes
        // (…16, 4, 1), a DDS in 87408 (…16, 16, 16).
        let levels = 9;
        let game = ytd_chain_size(TextureFormat::DXT5, 256, 256, levels);
        let dds = mip_chain_size(TextureFormat::DXT5, 256, 256, levels);
        assert_eq!((game, dds), (87381, 87408));
        let mut tex = tex("t", TextureFormat::DXT5, 256, 256);
        tex.levels = levels;
        tex.pixel_data = (0..game).map(|i| (i % 251) as u8).collect();

        let bytes = serialize_ytd(&[tex.clone()]).unwrap();
        let back = &parse_ytd(&bytes).unwrap()[0];
        assert_eq!(back.pixel_data, tex.pixel_data);

        let dds_bytes = back.to_dds();
        assert_eq!(dds_bytes.len(), 128 + dds);
        let from_dds = crate::parse_dds(&dds_bytes).unwrap();
        assert_eq!(from_dds.levels, levels);
        assert_eq!(from_dds.pixel_data, tex.pixel_data);

        // A DDS written short (the game's sizes, as older exports did) is taken as it is.
        let mut short = dds_bytes[..128].to_vec();
        short.extend_from_slice(&tex.pixel_data);
        assert_eq!(crate::parse_dds(&short).unwrap().pixel_data, tex.pixel_data);
        // Extra bytes beyond the game's size are dropped on write.
        let mut long = tex.clone();
        long.pixel_data.extend_from_slice(&[9; 27]);
        let bytes = serialize_ytd(&[long]).unwrap();
        assert_eq!(parse_ytd(&bytes).unwrap()[0].pixel_data, tex.pixel_data);
    }

    #[test]
    fn serialize_handles_an_empty_dictionary() {
        let bytes = serialize_ytd(&[]).unwrap();
        assert!(parse_ytd(&bytes).unwrap().is_empty());
    }

    #[test]
    fn serialize_rejects_duplicate_names_and_short_data() {
        let err = serialize_ytd(&[tex("a", TextureFormat::DXT1, 8, 8), tex("A", TextureFormat::DXT1, 8, 8)]).unwrap_err();
        assert!(err.to_string().contains("share the name hash"), "{err}");
        let mut short = tex("s", TextureFormat::DXT5, 16, 16);
        short.pixel_data.truncate(3);
        let err = serialize_ytd(&[short]).unwrap_err();
        assert!(err.to_string().contains("needs"), "{err}");
    }
}
