use anyhow::{bail, Result};
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::io::Read;
use crate::math::{Vec3, Vec4};

/// "RSC7": the header on every GTA V resource file (.ydr/.ytd/.ybn/.ynv…).
pub const RSC7_MAGIC: u32 = 0x37435352;
/// "RSC8": the Gen9 resource header, not supported by this crate.
pub const RSC8_MAGIC: u32 = 0x38435352;
/// "FXAP": the header Cfx.re's asset escrow puts on an encrypted FiveM
/// resource file. Its contents are not a RAGE resource at all, so nothing in
/// this crate can read one — the point of recognising it is to say so.
pub const FXAP_MAGIC: u32 = 0x5041_5846;

/// True when `data` is an escrowed (encrypted) FiveM asset rather than a
/// resource this crate can parse.
pub fn is_fxap(data: &[u8]) -> bool {
    data.len() >= 4 && u32::from_le_bytes(data[0..4].try_into().unwrap_or([0; 4])) == FXAP_MAGIC
}

/// The resource version encoded in the two RSC7 flag words (system nibble
/// high, graphics nibble low), e.g. 165 for a .ydr.
pub fn resource_version_from_flags(sys_flags: u32, gfx_flags: u32) -> u32 {
    let sv = (sys_flags  >> 28) & 0xF;
    let gv = (gfx_flags  >> 28) & 0xF;
    (sv << 4) | gv
}

/// Decodes an RSC7 flag word into the byte size of the section it describes.
pub fn resource_size_from_flags(flags: u32) -> usize {
    let s0 = ((flags >> 27) & 0x1)  << 0;
    let s1 = ((flags >> 26) & 0x1)  << 1;
    let s2 = ((flags >> 25) & 0x1)  << 2;
    let s3 = ((flags >> 24) & 0x1)  << 3;
    let s4 = ((flags >> 17) & 0x7F) << 4;
    let s5 = ((flags >> 11) & 0x3F) << 5;
    let s6 = ((flags >> 7)  & 0xF)  << 6;
    let s7 = ((flags >> 5)  & 0x3)  << 7;
    let s8 = ((flags >> 4)  & 0x1)  << 8;
    let ss = (flags & 0xF) as usize;
    let base_size = 0x200usize << ss;
    base_size * (s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7 + s8) as usize
}

pub const SYSTEM_BASE: u64 = 0x5000_0000;
pub const GRAPHICS_BASE: u64 = 0x6000_0000;

// ─── Internal virtual-memory reader ──────────────────────────────────────────

pub struct ResReader<'a> {
    pub system:   &'a [u8],
    pub graphics: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    System,
    Graphics,
}

/// Header of a `atArray`/pointer-list style structure: a pointer to the
/// backing array, followed by a `u16` count and a `u16` capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerListHeader {
    pub pointer: u64,
    pub count: u16,
    pub capacity: u16,
}

impl<'a> ResReader<'a> {
    /// Which section a virtual address points into, and the offset within it.
    /// Both bases share bit 30; bit 29 marks graphics and bit 28 system. An
    /// address with all three set counts as graphics, as it always has.
    fn locate(&self, va: u64) -> Option<(Section, usize)> {
        if va & GRAPHICS_BASE == GRAPHICS_BASE {
            Some((Section::Graphics, (va - GRAPHICS_BASE) as usize))
        } else if va & SYSTEM_BASE == SYSTEM_BASE {
            Some((Section::System, (va - SYSTEM_BASE) as usize))
        } else {
            None
        }
    }

    fn section(&self, which: Section) -> &'a [u8] {
        match which {
            Section::System => self.system,
            Section::Graphics => self.graphics,
        }
    }

    pub fn resolve(&self, va: u64, len: usize) -> Option<&'a [u8]> {
        if va == 0 { return None; }
        let (section, off) = self.locate(va)?;
        self.section(section).get(off..off.checked_add(len)?)
    }

    /// Like [`Self::resolve`], but distinguishes a null pointer (`va == 0`,
    /// returns `Some(None)`) from an out-of-bounds pointer (`None`).
    pub fn resolve_optional(&self, va: u64, len: usize) -> Option<Option<&'a [u8]>> {
        if va == 0 {
            return Some(None);
        }
        self.resolve(va, len).map(Some)
    }

    pub fn read_u16_list(&self, va: u64, count: usize) -> Option<Vec<u16>> {
        if count == 0 || va == 0 {
            return Some(Vec::new());
        }
        let bytes = self.resolve(va, count.checked_mul(2)?)?;
        Some(bytes.chunks_exact(2).map(|c| u16_le(c, 0)).collect())
    }

    pub fn read_u32_list(&self, va: u64, count: usize) -> Option<Vec<u32>> {
        if count == 0 || va == 0 {
            return Some(Vec::new());
        }
        let bytes = self.resolve(va, count.checked_mul(4)?)?;
        Some(bytes.chunks_exact(4).map(|c| u32_le(c, 0)).collect())
    }

    pub fn read_u64_list(&self, va: u64, count: usize) -> Option<Vec<u64>> {
        if count == 0 || va == 0 {
            return Some(Vec::new());
        }
        let bytes = self.resolve(va, count.checked_mul(8)?)?;
        Some(bytes.chunks_exact(8).map(|c| u64_le(c, 0)).collect())
    }

    /// Reads a 16-byte pointer-list header: pointer@0, count@8, capacity@10.
    pub fn read_pointer_list_header(&self, va: u64) -> Option<PointerListHeader> {
        let bytes = self.resolve(va, 16)?;
        Some(PointerListHeader {
            pointer: u64_le(bytes, 0),
            count: u16_le(bytes, 8),
            capacity: u16_le(bytes, 10),
        })
    }

    /// Longest string this reads before giving up on finding a terminating
    /// NUL. Real names are short; a runaway scan across the rest of the
    /// section usually means the pointer is bogus, so it's treated as an
    /// unresolved string (`None`) rather than returned truncated.
    const MAX_STRING_LEN: usize = 256;

    /// Names only ever live in the system section; a graphics-section
    /// address is treated as unresolved.
    pub fn string_at(&self, va: u64) -> Option<String> {
        let (Section::System, off) = self.locate(va)? else {
            return None;
        };
        let slice = self.system.get(off..)?;
        let scan_len = slice.len().min(Self::MAX_STRING_LEN);
        let end = slice[..scan_len].iter().position(|&b| b == 0)?;
        Some(String::from_utf8_lossy(&slice[..end]).into_owned())
    }
}

pub fn u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap_or([0; 2]))
}
pub fn u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap_or([0; 4]))
}
pub fn u64_le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap_or([0; 8]))
}
pub fn f32_le(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(b[off..off + 4].try_into().unwrap_or([0; 4]))
}
pub fn vec3_le(b: &[u8], off: usize) -> Vec3 {
    Vec3::new(f32_le(b, off), f32_le(b, off + 4), f32_le(b, off + 8))
}
pub fn vec4_le(b: &[u8], off: usize) -> Vec4 {
    Vec4::new(f32_le(b, off), f32_le(b, off + 4), f32_le(b, off + 8), f32_le(b, off + 12))
}

/// The system section alone of an RSC7 resource: the structs, names and
/// pointers, without the graphics pages that hold vertex and pixel data.
/// Reads only as far as the system section reaches, so a dictionary's
/// pixel data is never inflated.
pub fn prepare_rsc7_system(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        bail!("RSC7 data too short");
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != RSC7_MAGIC {
        bail!("Not an RSC7 file (magic = 0x{:08X})", magic);
    }
    let system_flags = u32::from_le_bytes(data[8..12].try_into().unwrap());
    inflate_rsc7_system(&data[16..], system_flags)
}

/// Inflates the first `resource_size_from_flags(system_flags)` bytes of an
/// RSC7 body (everything after the 16-byte header) and stops there. A body
/// that is not a deflate stream is taken as stored.
pub fn inflate_rsc7_system(body: &[u8], system_flags: u32) -> Result<Vec<u8>> {
    let sys_size = resource_size_from_flags(system_flags);
    let mut out = Vec::with_capacity(sys_size);
    match DeflateDecoder::new(body).take(sys_size as u64).read_to_end(&mut out) {
        Ok(_) if !out.is_empty() => {}
        Ok(_) => out = body.get(..sys_size.min(body.len())).unwrap_or(&[]).to_vec(),
        Err(_) if out.is_empty() => out = body.get(..sys_size.min(body.len())).unwrap_or(&[]).to_vec(),
        Err(_) => bail!(
            "corrupt deflate stream: inflated {} of an expected {} system bytes before failing",
            out.len(),
            sys_size
        ),
    }
    if out.len() < sys_size {
        bail!("Decompressed size {} < expected system size {}", out.len(), sys_size);
    }
    Ok(out)
}

/// Helper to decompress and prepare RSC7 resource sections.
pub fn prepare_rsc7(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if data.len() < 16 {
        bail!("RSC7 data too short");
    }

    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != RSC7_MAGIC {
        bail!("Not an RSC7 file (magic = 0x{:08X})", magic);
    }

    let system_flags  = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let graphics_flags = u32::from_le_bytes(data[12..16].try_into().unwrap());

    let sys_size  = resource_size_from_flags(system_flags);
    let gfx_size  = resource_size_from_flags(graphics_flags);
    let body      = &data[16..];

    // Most resources are deflated, but a few are stored raw. Telling the two
    // apart by whether inflate succeeds is fine; what matters is not confusing
    // a *corrupt* stream for a stored one, because feeding the still-compressed
    // bytes on as though they were the resource produces wild pointers far
    // downstream instead of naming the real problem here.
    let decompressed = {
        let mut out = Vec::new();
        match DeflateDecoder::new(body).read_to_end(&mut out) {
            Ok(_) if !out.is_empty() => out,
            Ok(_) => body.to_vec(),
            // Never looked like deflate at all — treat it as stored.
            Err(_) if out.is_empty() => body.to_vec(),
            Err(_) => bail!(
                "corrupt deflate stream: inflated {} of an expected {} bytes before failing",
                out.len(),
                sys_size + gfx_size
            ),
        }
    };

    if decompressed.len() < sys_size {
        bail!(
            "Decompressed size {} < expected system size {}",
            decompressed.len(), sys_size
        );
    }

    let system = decompressed[..sys_size].to_vec();
    let graphics = if decompressed.len() >= sys_size + gfx_size {
        decompressed[sys_size..sys_size + gfx_size].to_vec()
    } else {
        decompressed[sys_size..].to_vec()
    };

    Ok((system, graphics))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a system buffer with a 16-byte pointer-list header at offset 0
    /// (pointer -> 0x100 within the system section, count=3, capacity=4)
    /// followed by three little-endian u32s at 0x50000100.
    fn build_system_buffer() -> Vec<u8> {
        let mut sys = vec![0u8; 0x200];

        // Pointer-list header at offset 0.
        let array_va = SYSTEM_BASE + 0x100;
        sys[0..8].copy_from_slice(&array_va.to_le_bytes());
        sys[8..10].copy_from_slice(&3u16.to_le_bytes());
        sys[10..12].copy_from_slice(&4u16.to_le_bytes());

        // Three u32s at 0x100.
        sys[0x100..0x104].copy_from_slice(&11u32.to_le_bytes());
        sys[0x104..0x108].copy_from_slice(&22u32.to_le_bytes());
        sys[0x108..0x10C].copy_from_slice(&33u32.to_le_bytes());

        sys
    }

    #[test]
    fn resolve_optional_null_out_of_bounds_and_valid() {
        let sys = build_system_buffer();
        let reader = ResReader { system: &sys, graphics: &[] };

        // va == 0 -> Some(None)
        assert_eq!(reader.resolve_optional(0, 4), Some(None));

        // Out of bounds -> None
        let far_va = SYSTEM_BASE + sys.len() as u64 + 0x1000;
        assert_eq!(reader.resolve_optional(far_va, 4), None);

        // Valid -> Some(Some(bytes))
        let array_va = SYSTEM_BASE + 0x100;
        let resolved = reader.resolve_optional(array_va, 4).expect("should resolve");
        let bytes = resolved.expect("should be Some(bytes)");
        assert_eq!(u32_le(bytes, 0), 11);
    }

    #[test]
    fn read_u32_list_reads_values() {
        let sys = build_system_buffer();
        let reader = ResReader { system: &sys, graphics: &[] };

        let array_va = SYSTEM_BASE + 0x100;
        let values = reader.read_u32_list(array_va, 3).expect("should read list");
        assert_eq!(values, vec![11, 22, 33]);

        // count == 0 -> empty vec, even for a null va.
        assert_eq!(reader.read_u32_list(0, 0), Some(Vec::new()));
    }

    #[test]
    fn read_lists_reject_counts_that_would_overflow_the_byte_length() {
        let sys = build_system_buffer();
        let reader = ResReader { system: &sys, graphics: &[] };
        let array_va = SYSTEM_BASE + 0x100;

        // On a 32-bit `usize` (wasm32), `count * N` wraps around instead of
        // overflowing, so pick a count that overflows even a 64-bit `usize`
        // multiplication to make the assertion hold on every target.
        let huge_count = usize::MAX / 4 + 1;

        assert_eq!(reader.read_u16_list(array_va, huge_count), None);
        assert_eq!(reader.read_u32_list(array_va, huge_count), None);
        assert_eq!(reader.read_u64_list(array_va, huge_count), None);
    }

    #[test]
    fn resolve_rejects_offset_length_overflow_instead_of_panicking() {
        let reader = ResReader { system: &[], graphics: &[] };

        // A `va` with bit 29 (system) or bit 30 (graphics) set near `u64::MAX`
        // plus a huge `len` used to overflow `off + len` in debug builds.
        assert_eq!(reader.resolve(u64::MAX, usize::MAX), None);
    }

    #[test]
    fn string_at_gives_up_past_the_scan_cap_instead_of_returning_untruncated() {
        // No NUL anywhere in a buffer larger than the scan cap -> None, not a
        // huge (or truncated) string.
        let sys = vec![b'A'; 512];
        let reader = ResReader { system: &sys, graphics: &[] };
        assert_eq!(reader.string_at(SYSTEM_BASE), None);

        // A NUL just past the cap still isn't found.
        let mut sys = vec![b'A'; 300];
        sys[300 - 1] = 0; // NUL at index 299, past the 256-byte scan window
        let reader = ResReader { system: &sys, graphics: &[] };
        assert_eq!(reader.string_at(SYSTEM_BASE), None);

        // A NUL within the cap still works normally.
        let mut sys = vec![b'A'; 10];
        sys[5] = 0;
        let reader = ResReader { system: &sys, graphics: &[] };
        assert_eq!(reader.string_at(SYSTEM_BASE), Some("AAAAA".to_string()));
    }

    #[test]
    fn read_pointer_list_header_reads_fields() {
        let sys = build_system_buffer();
        let reader = ResReader { system: &sys, graphics: &[] };

        let header = reader.read_pointer_list_header(SYSTEM_BASE).expect("should read header");
        assert_eq!(header.pointer, SYSTEM_BASE + 0x100);
        assert_eq!(header.count, 3);
        assert_eq!(header.capacity, 4);
    }
}

// ─── writing ─────────────────────────────────────────────────────────────────

/// The RSC7 flag word describing the smallest page set (largest pages first,
/// per-size counts capped as the format caps them) that holds `size` bytes.
/// The virtual size it encodes is what [`resource_size_from_flags`] returns,
/// so callers pad their section to that.
pub fn rsc7_flags_for_size(size: usize) -> Result<u32> {
    for ss in 0u32..16 {
        let base = 0x200usize << ss;
        let mut units = size.div_ceil(base);
        // (shift into the flag word, page size in units, max count)
        let fields: [(u32, usize, usize); 9] = [
            (4, 256, 1), (5, 128, 3), (7, 64, 15), (11, 32, 63), (17, 16, 127),
            (24, 8, 1), (25, 4, 1), (26, 2, 1), (27, 1, 1),
        ];
        let mut flags = ss;
        for (shift, page, cap) in fields {
            let n = (units / page).min(cap);
            units -= n * page;
            flags |= (n as u32) << shift;
        }
        if units == 0 {
            return Ok(flags);
        }
    }
    bail!("{size} bytes is too large for an RSC7 section");
}

/// The flag word for `count` pages of exactly `page_size` bytes each (a
/// power of two times 0x200). Every block in the section must then sit
/// inside one page: the game maps pages as separate allocations.
pub fn rsc7_flags_for_pages(page_size: usize, count: usize) -> Result<u32> {
    // (shift into the flag word, page size in base units, max count)
    let fields: [(u32, usize, usize); 9] = [
        (27, 1, 1), (26, 2, 1), (25, 4, 1), (24, 8, 1), (17, 16, 127), (11, 32, 63), (7, 64, 15), (5, 128, 3), (4, 256, 1),
    ];
    for ss in 0u32..16 {
        let base = 0x200usize << ss;
        if page_size < base { break; }
        for (shift, units, cap) in fields {
            if base * units == page_size && count <= cap {
                return Ok(ss | ((count as u32) << shift));
            }
        }
    }
    bail!("{count} pages of {page_size} bytes cannot be described by RSC7 flags");
}

/// [`build_rsc7`] for a system section laid out in equal pages of
/// `page_size` bytes (no graphics section): the section is padded to a
/// whole number of pages and the flags describe exactly those pages.
pub fn build_rsc7_paged(version: u32, system: &[u8], page_size: usize) -> Result<Vec<u8>> {
    let pages = system.len().div_ceil(page_size).max(1);
    let sys_flags = rsc7_flags_for_pages(page_size, pages)? | ((version >> 4) & 0xF) << 28;
    let gfx_flags = (version & 0xF) << 28;
    let mut body = system.to_vec();
    body.resize(pages * page_size, 0);
    Ok(wrap_rsc7(version, sys_flags, gfx_flags, &body))
}

/// Wraps a system (and optional graphics) section in an RSC7 header with the
/// given resource version and a deflated body, padding each section to the
/// page layout its flags describe. Version is the pair of nibbles
/// [`resource_version_from_flags`] reads back (e.g. 2 for a .ynv, 165 for a .ydr).
pub fn build_rsc7(version: u32, system: &[u8], graphics: &[u8]) -> Vec<u8> {
    let sys_flags = rsc7_flags_for_size(system.len().max(1)).expect("section fits");
    let gfx_flags = if graphics.is_empty() { 0 } else { rsc7_flags_for_size(graphics.len()).expect("section fits") };
    build_rsc7_with_flags(version, sys_flags, system, gfx_flags, graphics)
}

/// [`build_rsc7`] with the page flags chosen by the caller (the version
/// nibbles are added here), for sections laid out by [`pack_pages`].
pub fn build_rsc7_with_flags(version: u32, sys_flags: u32, system: &[u8], gfx_flags: u32, graphics: &[u8]) -> Vec<u8> {
    let sys_flags = (sys_flags & 0x0FFF_FFFF) | ((version >> 4) & 0xF) << 28;
    let gfx_flags = (gfx_flags & 0x0FFF_FFFF) | (version & 0xF) << 28;
    let sys_size = resource_size_from_flags(sys_flags);
    let gfx_size = resource_size_from_flags(gfx_flags);
    assert!(system.len() <= sys_size && graphics.len() <= gfx_size, "sections exceed their page flags");
    let mut body = Vec::with_capacity(sys_size + gfx_size);
    body.extend_from_slice(system);
    body.resize(sys_size, 0);
    body.extend_from_slice(graphics);
    body.resize(sys_size + gfx_size, 0);

    wrap_rsc7(version, sys_flags, gfx_flags, &body)
}

/// Number of pages an RSC7 flag word describes.
pub fn rsc7_page_count(flags: u32) -> usize {
    let tail = ((flags >> 24) & 0xF).count_ones() as usize;
    let sized = ((flags >> 4) & 0x1) + ((flags >> 5) & 0x3) + ((flags >> 7) & 0xF) + ((flags >> 11) & 0x3F) + ((flags >> 17) & 0x7F);
    tail + sized as usize
}

/// Where each block of a section landed after [`pack_pages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedLayout {
    /// RSC7 flags (page sizes and counts, no version nibble) for the section.
    pub flags: u32,
    /// Byte offset of each input block from the start of the section.
    pub offsets: Vec<usize>,
    /// Total size of the section, the sum of its pages.
    pub size: usize,
}

/// Packs blocks of the given sizes into RSC7 pages the way the reference tool's
/// `ResourceBuilder` does. The game maps each page as its own allocation,
/// so no block may straddle two pages; blocks are placed first-fit into
/// pages of five sizes (`0x2000 << shift` times 1, 2, 4, 8 and 16), largest
/// blocks first and 16-byte aligned. The base shift starts at the smallest
/// value where the base page is no smaller than the smallest block and the
/// biggest page size (16x) holds the largest block
/// (`ResourceBuilder.cs:363-368`), then grows until the per-size page counts
/// fit the flag word and `max_pages` in total.
///
/// With `root_first` the first block is placed at offset 0 regardless of
/// its size, as a resource's root struct must be.
pub fn pack_pages(sizes: &[usize], root_first: bool, max_pages: usize) -> Result<PagedLayout> {
    const ALIGN: usize = 16;
    const CAPS: [usize; 5] = [0x7F, 0x3F, 0xF, 0x3, 0x1];
    const FLAG_SHIFTS: [u32; 5] = [17, 11, 7, 5, 4];

    if sizes.is_empty() {
        return Ok(PagedLayout { flags: 0, offsets: vec![], size: 0 });
    }
    let max_block = *sizes.iter().max().unwrap();
    let min_block = *sizes.iter().min().unwrap();
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    let sortable = if root_first { &mut order[1..] } else { &mut order[..] };
    sortable.sort_by(|a, b| sizes[*b].cmp(&sizes[*a]));

    for shift in 0u32..16 {
        let base = 0x2000usize << shift;
        if base < min_block || base * 16 < max_block {
            continue;
        }
        let mut top = 0;
        while (base << top) < max_block {
            top += 1;
        }
        // Fill level of every page, per page size.
        let mut pages: [Vec<usize>; 5] = Default::default();
        let mut place = vec![(0usize, 0usize, 0usize); sizes.len()];
        for (k, &i) in order.iter().enumerate() {
            let size = sizes[i];
            if k == 0 {
                pages[top].push(size);
                place[i] = (top, 0, 0);
                continue;
            }
            let mut want = 0;
            while size > (base << want) && want < top {
                want += 1;
            }
            let mut found = false;
            'pages: for t in want..=top {
                for (p, fill) in pages[t].iter_mut().enumerate() {
                    let at = fill.next_multiple_of(ALIGN);
                    if at + size <= (base << t) {
                        *fill = at + size;
                        place[i] = (t, p, at);
                        found = true;
                        break 'pages;
                    }
                }
            }
            if !found {
                place[i] = (want, pages[want].len(), 0);
                pages[want].push(size);
            }
        }
        let counts: Vec<usize> = pages.iter().map(Vec::len).collect();
        let fits = counts.iter().zip(CAPS).all(|(c, cap)| *c <= cap) && counts.iter().sum::<usize>() <= max_pages;
        if !fits {
            continue;
        }
        let mut page_base = [0usize; 5];
        let mut size = 0;
        for t in (0..5).rev() {
            page_base[t] = size;
            size += (base << t) * counts[t];
        }
        let offsets = place.iter().map(|&(t, p, at)| page_base[t] + (base << t) * p + at).collect();
        let mut flags = shift;
        for (t, fs) in FLAG_SHIFTS.iter().enumerate() {
            flags |= (counts[t] as u32) << fs;
        }
        return Ok(PagedLayout { flags, offsets, size });
    }
    bail!("{} blocks ({} bytes in the largest) do not fit in {} RSC7 pages", sizes.len(), max_block, max_pages)
}

fn wrap_rsc7(version: u32, sys_flags: u32, gfx_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&RSC7_MAGIC.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&sys_flags.to_le_bytes());
    out.extend_from_slice(&gfx_flags.to_le_bytes());
    let mut enc = DeflateEncoder::new(out, Compression::best());
    std::io::Write::write_all(&mut enc, body).expect("writing to a Vec cannot fail");
    enc.finish().expect("writing to a Vec cannot fail")
}

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn recognises_an_fxap_escrow_header() {
        assert!(is_fxap(b"FXAPsome encrypted payload"));
        assert_eq!(FXAP_MAGIC, u32::from_le_bytes(*b"FXAP"));
        // Anything else, including a real resource and a too-short file.
        assert!(!is_fxap(&RSC7_MAGIC.to_le_bytes()));
        assert!(!is_fxap(b"FXA"));
        assert!(!is_fxap(b""));
    }

    #[test]
    fn flags_round_trip_through_size() {
        for size in [1usize, 512, 8192, 24576 + 180224 + 32768, 237568, 1 << 20, 3_000_001] {
            let flags = rsc7_flags_for_size(size).unwrap();
            let virt = resource_size_from_flags(flags);
            assert!(virt >= size, "{size}: {virt}");
            assert!(virt < size + (0x200usize << (flags & 0xF)) * 256, "{size}: wasteful {virt}");
        }
        // The retail navmesh[108][96].ynv layout.
        assert_eq!(resource_size_from_flags(0x0006_5880), 237568);
    }

    #[test]
    fn page_flags_describe_equal_pages() {
        let flags = rsc7_flags_for_pages(16384, 8).unwrap();
        assert_eq!(resource_size_from_flags(flags), 8 * 16384);
        let file = build_rsc7_paged(2, &[7u8; 40000], 16384).unwrap();
        let sys_flags = u32::from_le_bytes(file[8..12].try_into().unwrap());
        assert_eq!(resource_size_from_flags(sys_flags), 3 * 16384);
        assert!(rsc7_flags_for_pages(16384, 10_000).is_err());
    }

    #[test]
    fn build_rsc7_round_trips() {
        let system: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        let file = build_rsc7(2, &system, &[]);
        assert_eq!(u32::from_le_bytes(file[0..4].try_into().unwrap()), RSC7_MAGIC);
        let sys_flags = u32::from_le_bytes(file[8..12].try_into().unwrap());
        let gfx_flags = u32::from_le_bytes(file[12..16].try_into().unwrap());
        assert_eq!(resource_version_from_flags(sys_flags, gfx_flags), 2);
        let (sys, gfx) = prepare_rsc7(&file).unwrap();
        assert_eq!(&sys[..system.len()], &system[..]);
        assert!(gfx.is_empty());
    }
}

#[cfg(test)]
mod pack_tests {
    use super::*;

    #[test]
    fn small_blocks_share_one_base_page_with_the_root_first() {
        let layout = pack_pages(&[0x40, 0x410, 20, 40, 0x90, 0x90, 7], true, 128).unwrap();
        assert_eq!(layout.offsets[0], 0);
        assert_eq!(layout.size, 0x2000);
        assert_eq!(rsc7_page_count(layout.flags), 1);
        assert_eq!(resource_size_from_flags(layout.flags), 0x2000);
        // Every block is 16-aligned and inside the section, none overlap.
        let sizes = [0x40, 0x410, 20, 40, 0x90, 0x90, 7];
        let mut spans: Vec<(usize, usize)> = layout.offsets.iter().zip(sizes).map(|(&o, s)| (o, o + s)).collect();
        spans.sort();
        for w in spans.windows(2) {
            assert!(w[0].1 <= w[1].0, "{spans:?}");
        }
        assert!(layout.offsets.iter().all(|o| o % 16 == 0));
    }

    #[test]
    fn no_block_straddles_a_page() {
        let sizes = [349520, 220800, 349520, 349520, 360000, 16, 16, 43688];
        let layout = pack_pages(&sizes, false, 128).unwrap();
        assert_eq!(resource_size_from_flags(layout.flags), layout.size);
        let base = 0x2000usize << (layout.flags & 0xF);
        // Reconstruct page boundaries: sizes largest first.
        let counts = [(layout.flags >> 17) & 0x7F, (layout.flags >> 11) & 0x3F, (layout.flags >> 7) & 0xF, (layout.flags >> 5) & 0x3, (layout.flags >> 4) & 0x1];
        let mut bounds = vec![];
        let mut at = 0;
        for t in (0..5).rev() {
            for _ in 0..counts[t] {
                bounds.push((at, at + (base << t)));
                at += base << t;
            }
        }
        for (&o, s) in layout.offsets.iter().zip(sizes) {
            assert!(bounds.iter().any(|&(lo, hi)| o >= lo && o + s <= hi), "block at {o} ({s} B) crosses a page: {bounds:?}");
        }
        assert!(layout.size <= sizes.iter().sum::<usize>() * 2, "packing wastes too much: {}", layout.size);
    }

    #[test]
    fn empty_input_is_an_empty_section() {
        assert_eq!(pack_pages(&[], true, 128).unwrap(), PagedLayout { flags: 0, offsets: vec![], size: 0 });
    }

    #[test]
    fn page_count_matches_size_arithmetic() {
        for flags in [0x00020000u32, 0x1080006, 0x0000a04, 0x0000040, 0x0000_0010, 0x0f00_0000] {
            let n = rsc7_page_count(flags);
            assert!(n >= 1 || resource_size_from_flags(flags) == 0, "{flags:08x}");
        }
        assert_eq!(rsc7_page_count(0x0000_0040), 2);
        assert_eq!(rsc7_page_count(0x0108_0006), 5);
    }
}
