//! The world heightmap (`heightmap.dat`, `heightmapheistisland.dat`): a
//! coarse grid of the highest and lowest ground over the map, one byte each
//! per cell, scaled into the file's bounding box. The game keeps it under
//! `common/data/levels/gta5/`; the retail one is 183x249 cells over
//! (-4050,-4050)..(5100,8400).
//!
//! Layout, from the reference library's `HeightmapFile.cs`: a 44-byte header
//! (`HMAP`, version 1.1, a pad, a compressed flag, width, height, the box
//! and the byte length of what follows), then one 8-byte row header per row
//! (`start`, `count`, `dataOffset`) and the row data: every row's max
//! heights packed end to end, then every row's min heights the same way,
//! each row holding only the cells from its first non-zero max height to
//! its last. Retail files are big-endian; a little-endian file announces
//! itself by its magic reading as `HMAP` the other way round.

use anyhow::{bail, Context, Result};

use crate::math::Vec3;

const MAGIC: u32 = 0x484D_4150; // 'HMAP'
const HEADER_LEN: usize = 44;
const ROW_HEADER_LEN: usize = 8;

/// A decoded heightmap. `max_heights` and `min_heights` are row-major,
/// `width * height` bytes, row 0 at `bb_min.y`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldHeightmap {
    pub version_major: u8,
    pub version_minor: u8,
    pub width: u16,
    pub height: u16,
    pub bb_min: Vec3,
    pub bb_max: Vec3,
    pub max_heights: Vec<u8>,
    pub min_heights: Vec<u8>,
    /// False for the retail (big-endian) files.
    pub little_endian: bool,
}

/// One row's slice of the packed data, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowHeader {
    start: u16,
    count: u16,
    data_offset: i32,
}

/// Reads scalars in whichever byte order the file uses.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    little_endian: bool,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        let out = self.data.get(self.pos..self.pos + n).context("heightmap: truncated")?;
        self.pos += n;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b: [u8; 2] = self.bytes(2)?.try_into().unwrap();
        Ok(if self.little_endian { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self.bytes(4)?.try_into().unwrap();
        Ok(if self.little_endian { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
}

/// Parses a `heightmap*.dat` file.
pub fn parse_heightmap(data: &[u8]) -> Result<WorldHeightmap> {
    if data.len() < HEADER_LEN {
        bail!("heightmap: {} bytes is too short for the header", data.len());
    }
    // `HeightmapFile.Load`: the magic read little-endian says which way
    // round the file is; the retail files are big-endian.
    let little_endian = u32::from_le_bytes(data[..4].try_into().unwrap()) == MAGIC;
    let mut r = Reader { data, pos: 0, little_endian };
    let magic = r.u32()?;
    if magic != MAGIC {
        bail!("heightmap: bad magic 0x{magic:08X}");
    }
    let version_major = r.u8()?;
    let version_minor = r.u8()?;
    let _pad = r.u16()?;
    let compressed = r.u32()?;
    let width = r.u16()?;
    let height = r.u16()?;
    let bb_min = r.vec3()?;
    let bb_max = r.vec3()?;
    let length = r.u32()? as usize;
    if length != data.len() - r.pos {
        bail!("heightmap: the header says {length} bytes follow but {} do", data.len() - r.pos);
    }

    let cells = width as usize * height as usize;
    let (max_heights, min_heights) = if compressed > 0 {
        let mut rows = Vec::with_capacity(height as usize);
        for _ in 0..height {
            rows.push(RowHeader { start: r.u16()?, count: r.u16()?, data_offset: r.u32()? as i32 });
        }
        let dlen = length - height as usize * ROW_HEADER_LEN;
        let d = r.bytes(dlen)?;
        // The max rows come first, the min rows after them, each half as
        // long as the whole (`var h2off = dlen / 2`).
        let h2off = dlen / 2;
        let mut max_heights = vec![0u8; cells];
        let mut min_heights = vec![0u8; cells];
        for (y, row) in rows.iter().enumerate() {
            for i in 0..row.count as usize {
                let x = row.start as usize + i;
                // `int o = h.DataOffset + x`: the offset is negative when a
                // row starts past where its data begins.
                let o = usize::try_from(row.data_offset as i64 + x as i64).context("heightmap: row offset before the data")?;
                let cell = y * width as usize + x;
                let (Some(&hi), Some(&lo), Some(max), Some(min)) =
                    (d.get(o), d.get(o + h2off), max_heights.get_mut(cell), min_heights.get_mut(cell))
                else {
                    bail!("heightmap: row {y} reaches past the data");
                };
                *max = hi;
                *min = lo;
            }
        }
        (max_heights, min_heights)
    } else {
        // Never seen: every vanilla heightmap is compressed. The reference tool
        // keeps the raw bytes as both grids, so this does the same.
        let d = r.bytes(length)?.to_vec();
        (d.clone(), d)
    };
    Ok(WorldHeightmap { version_major, version_minor, width, height, bb_min, bb_max, max_heights, min_heights, little_endian })
}

/// Writes a heightmap back the way `HeightmapFile.Write` does: compressed,
/// each row trimmed to its first and last non-zero max height.
pub fn serialize_heightmap(h: &WorldHeightmap) -> Result<Vec<u8>> {
    let (w, rows) = (h.width as usize, h.height as usize);
    if h.max_heights.len() != w * rows || h.min_heights.len() != w * rows {
        bail!("heightmap: {}x{} cells but {} max and {} min heights", h.width, h.height, h.max_heights.len(), h.min_heights.len());
    }
    let mut headers = Vec::with_capacity(rows);
    let mut d1 = Vec::new();
    let mut d2 = Vec::new();
    for y in 0..rows {
        let row = &h.max_heights[y * w..(y + 1) * w];
        let start = row.iter().position(|&v| v != 0).unwrap_or(0);
        let end = row.iter().rposition(|&v| v != 0).map_or(0, |x| x + 1);
        let count = end.saturating_sub(start);
        let offset = if count > 0 { d1.len() as i32 - start as i32 } else { 0 };
        d1.extend_from_slice(&h.max_heights[y * w + start..y * w + start + count]);
        d2.extend_from_slice(&h.min_heights[y * w + start..y * w + start + count]);
        headers.push(RowHeader { start: start as u16, count: count as u16, data_offset: offset });
    }
    d1.extend_from_slice(&d2);
    let length = d1.len() + rows * ROW_HEADER_LEN;

    let mut out = Vec::with_capacity(HEADER_LEN + length);
    let le = h.little_endian;
    let put_u16 = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
    let put_u32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
    let put_vec3 = |out: &mut Vec<u8>, v: Vec3| {
        for f in [v.x, v.y, v.z] {
            put_u32(out, f.to_bits());
        }
    };
    put_u32(&mut out, MAGIC);
    out.push(h.version_major);
    out.push(h.version_minor);
    put_u16(&mut out, 0);
    put_u32(&mut out, 1);
    put_u16(&mut out, h.width);
    put_u16(&mut out, h.height);
    put_vec3(&mut out, h.bb_min);
    put_vec3(&mut out, h.bb_max);
    put_u32(&mut out, length as u32);
    for row in headers {
        put_u16(&mut out, row.start);
        put_u16(&mut out, row.count);
        put_u32(&mut out, row.data_offset as u32);
    }
    out.extend_from_slice(&d1);
    Ok(out)
}

impl WorldHeightmap {
    /// The world-space size of one cell: the box divided by the cell counts
    /// less one, as `Heightmaps.BuildHeightmapVertices` steps (`siz / (w - 1,
    /// h - 1)`).
    pub fn cell_size(&self) -> (f32, f32) {
        let steps = |n: u16| (n.max(2) - 1) as f32;
        ((self.bb_max.x - self.bb_min.x) / steps(self.width), (self.bb_max.y - self.bb_min.y) / steps(self.height))
    }

    /// The world x, y of cell `(ix, iy)`'s sample point.
    pub fn position_of(&self, ix: usize, iy: usize) -> (f32, f32) {
        let (sx, sy) = self.cell_size();
        (self.bb_min.x + ix as f32 * sx, self.bb_min.y + iy as f32 * sy)
    }

    /// A stored byte as a world height: 0 is the box's bottom, 255 its top.
    pub fn height_of(&self, value: u8) -> f32 {
        self.bb_min.z + value as f32 * (self.bb_max.z - self.bb_min.z) / 255.0
    }

    /// The highest ground in cell `(ix, iy)`.
    pub fn max_height_at(&self, ix: usize, iy: usize) -> Option<f32> {
        self.sample(&self.max_heights, ix, iy)
    }

    /// The lowest ground in cell `(ix, iy)`.
    pub fn min_height_at(&self, ix: usize, iy: usize) -> Option<f32> {
        self.sample(&self.min_heights, ix, iy)
    }

    fn sample(&self, grid: &[u8], ix: usize, iy: usize) -> Option<f32> {
        if ix >= self.width as usize || iy >= self.height as usize {
            return None;
        }
        grid.get(iy * self.width as usize + ix).map(|&v| self.height_of(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WorldHeightmap {
        // A 4x3 grid: row 0 all sea (zero), row 1 a hill in the middle, row
        // 2 land right up to the edge.
        WorldHeightmap {
            version_major: 1,
            version_minor: 1,
            width: 4,
            height: 3,
            bb_min: Vec3::new(-100.0, -50.0, 0.0),
            bb_max: Vec3::new(200.0, 50.0, 510.0),
            max_heights: vec![0, 0, 0, 0, 0, 10, 20, 0, 5, 6, 7, 8],
            min_heights: vec![0, 0, 0, 0, 0, 2, 3, 0, 1, 1, 1, 1],
            little_endian: false,
        }
    }

    #[test]
    fn a_heightmap_round_trips_in_either_byte_order() {
        for le in [false, true] {
            let h = WorldHeightmap { little_endian: le, ..sample() };
            let bytes = serialize_heightmap(&h).unwrap();
            assert_eq!(&bytes[..4], if le { b"PAMH" } else { b"HMAP" });
            // 44 header + 3 rows * 8 + (0 + 2 + 4) cells * 2 grids.
            assert_eq!(bytes.len(), 44 + 24 + 12);
            let back = parse_heightmap(&bytes).unwrap();
            assert_eq!(back, h);
        }
    }

    #[test]
    fn rows_are_trimmed_to_their_non_zero_span() {
        let bytes = serialize_heightmap(&sample()).unwrap();
        // Row 1's header: start 1, count 2, offset = data position - start = 0 - 1.
        let row1 = &bytes[44 + 8..44 + 16];
        assert_eq!(u16::from_be_bytes([row1[0], row1[1]]), 1);
        assert_eq!(u16::from_be_bytes([row1[2], row1[3]]), 2);
        assert_eq!(i32::from_be_bytes([row1[4], row1[5], row1[6], row1[7]]), -1);
        // Row 2 starts where row 1's two bytes end: offset 2 - 0.
        let row2 = &bytes[44 + 16..44 + 24];
        assert_eq!(u16::from_be_bytes([row2[2], row2[3]]), 4);
        assert_eq!(i32::from_be_bytes([row2[4], row2[5], row2[6], row2[7]]), 2);
    }

    #[test]
    fn heights_are_scaled_into_the_box() {
        let h = sample();
        assert_eq!(h.cell_size(), (100.0, 50.0));
        assert_eq!(h.position_of(0, 0), (-100.0, -50.0));
        assert_eq!(h.position_of(3, 2), (200.0, 50.0));
        assert_eq!(h.max_height_at(2, 1), Some(40.0), "20 of 255 over 510 m");
        assert_eq!(h.min_height_at(2, 1), Some(6.0));
        assert_eq!(h.max_height_at(0, 0), Some(0.0));
        assert_eq!(h.max_height_at(4, 0), None);
    }

    #[test]
    fn a_wrong_length_is_refused() {
        let mut bytes = serialize_heightmap(&sample()).unwrap();
        bytes.push(0);
        assert!(parse_heightmap(&bytes).unwrap_err().to_string().contains("bytes follow"));
        assert!(parse_heightmap(b"nope").unwrap_err().to_string().contains("too short"));
    }
}
