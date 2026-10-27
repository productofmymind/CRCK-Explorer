// Reads the game's world cache files, `gta5_cache_y.dat` and each DLC
// pack's `cacheloaderdata_dlc/<pack>_<hash>_cache_y.dat`: what the game
// knows about every map file, interior and collision file without opening
// them.
//
// Layout: a `[VERSION]\n<n>\n` header NUL-padded to 100 bytes, then text
// lines. `<fileDates>` .. `</fileDates>` holds one `<hash> <timestamp>
// [<id>]` line per archive. A line reading `fwMapDataStore`,
// `CInteriorProxy` or `BoundsStore` is followed by a little-endian u32 byte
// length and that many bytes of fixed-size records (64, 104 and 32 bytes).
// The first NUL after the header ends the file.
//
// Ported from the reference library's GameFiles/FileTypes/CacheDatFile.cs.

use anyhow::{bail, Context, Result};

use crate::math::{Vec3, Vec4};

/// One map file (`.ymap`) as the cache records it: the name hash it is
/// streamed by, its parent map, and its extents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapDataNode {
    pub name: u32,
    pub parent: u32,
    pub content_flags: u32,
    pub streaming_min: Vec3,
    pub streaming_max: Vec3,
    pub entities_min: Vec3,
    pub entities_max: Vec3,
    /// Four flag bytes the reference tool leaves unnamed (HD/LOD/SLOD guesses).
    pub flags: [u8; 4],
}

/// One placed interior: the MLO archetype's name hash, the map file that
/// places it, and where.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteriorProxy {
    /// Three leading words the reference tool leaves unnamed.
    pub unknown: [u32; 3],
    /// The interior's archetype name hash.
    pub name: u32,
    /// Name hash of the `.ymap` whose `CMloInstanceDef` places it.
    pub parent: u32,
    pub position: Vec3,
    /// Rotation as a quaternion, `x, y, z, w`.
    pub orientation: Vec4,
    pub bb_min: Vec3,
    pub bb_max: Vec3,
    /// Four trailing words the reference tool leaves unnamed.
    pub trailing: [u64; 4],
}

/// One collision file (`.ybn`) and its bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundsStoreItem {
    pub name: u32,
    pub min: Vec3,
    pub max: Vec3,
    pub layer: u32,
}

/// One `<fileDates>` line: an archive's name hash and its timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheFileDate {
    pub file_name: u32,
    /// A Windows `FILETIME` (100 ns ticks since 1601).
    pub timestamp: i64,
    pub file_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheDat {
    pub version: String,
    pub file_dates: Vec<CacheFileDate>,
    pub map_nodes: Vec<MapDataNode>,
    pub interior_proxies: Vec<InteriorProxy>,
    pub bounds: Vec<BoundsStoreItem>,
}

const HEADER_LEN: usize = 100;
const MAP_NODE_LEN: usize = 64;
const INTERIOR_PROXY_LEN: usize = 104;
const BOUNDS_ITEM_LEN: usize = 32;

/// Parses a `*cache_y.dat` file.
pub fn parse_cache_dat(data: &[u8]) -> Result<CacheDat> {
    if !data.starts_with(b"[VERSION]") {
        bail!("cache_y.dat: missing [VERSION] header");
    }
    let header = &data[..data.len().min(HEADER_LEN)];
    let header = &header[..header.iter().position(|&b| b == 0).unwrap_or(header.len())];
    let version = String::from_utf8_lossy(header)
        .replace("[VERSION]", "")
        .replace(['\r', '\n'], "");

    let mut out = CacheDat { version, ..Default::default() };
    let mut in_dates = false;
    let mut line_start = HEADER_LEN;
    let mut i = HEADER_LEN;
    while i < data.len() {
        let b = data[i];
        if b == 0 {
            break;
        }
        if b != b'\n' {
            i += 1;
            continue;
        }
        let line = &data[line_start..i];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let record_len = match line {
            b"<fileDates>" => { in_dates = true; None }
            b"</fileDates>" => { in_dates = false; None }
            b"fwMapDataStore" => Some(MAP_NODE_LEN),
            b"CInteriorProxy" => Some(INTERIOR_PROXY_LEN),
            b"BoundsStore" => Some(BOUNDS_ITEM_LEN),
            _ => {
                if in_dates {
                    out.file_dates.extend(parse_date(line));
                }
                None
            }
        };
        i += 1;
        if let Some(size) = record_len {
            let name = String::from_utf8_lossy(line).into_owned();
            let len = data.get(i..i + 4).context("cache_y.dat: truncated block length")?;
            let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
            let body = data
                .get(i + 4..i + 4 + len)
                .with_context(|| format!("cache_y.dat: {name} block runs past the end of the file"))?;
            // The reference tool reads fixed-size records until the block ends; a
            // remainder shorter than one record is ignored the same way.
            for rec in body.chunks_exact(size) {
                match size {
                    MAP_NODE_LEN => out.map_nodes.push(map_node(rec)),
                    INTERIOR_PROXY_LEN => out.interior_proxies.push(interior_proxy(rec)),
                    _ => out.bounds.push(bounds_item(rec)),
                }
            }
            i += 4 + len;
        }
        line_start = i;
    }
    Ok(out)
}

fn parse_date(line: &[u8]) -> Option<CacheFileDate> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split(' ');
    let file_name = parts.next()?.parse().ok()?;
    let timestamp = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let file_id = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(CacheFileDate { file_name, timestamp, file_id })
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32_at(b, o))
}

fn vec3_at(b: &[u8], o: usize) -> Vec3 {
    Vec3::new(f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8))
}

fn map_node(b: &[u8]) -> MapDataNode {
    MapDataNode {
        name: u32_at(b, 0),
        parent: u32_at(b, 4),
        content_flags: u32_at(b, 8),
        streaming_min: vec3_at(b, 12),
        streaming_max: vec3_at(b, 24),
        entities_min: vec3_at(b, 36),
        entities_max: vec3_at(b, 48),
        flags: [b[60], b[61], b[62], b[63]],
    }
}

fn interior_proxy(b: &[u8]) -> InteriorProxy {
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    InteriorProxy {
        unknown: [u32_at(b, 0), u32_at(b, 4), u32_at(b, 8)],
        name: u32_at(b, 12),
        parent: u32_at(b, 16),
        position: vec3_at(b, 20),
        orientation: Vec4::new(f32_at(b, 32), f32_at(b, 36), f32_at(b, 40), f32_at(b, 44)),
        bb_min: vec3_at(b, 48),
        bb_max: vec3_at(b, 60),
        trailing: [u64_at(72), u64_at(80), u64_at(88), u64_at(96)],
    }
}

fn bounds_item(b: &[u8]) -> BoundsStoreItem {
    BoundsStoreItem { name: u32_at(b, 0), min: vec3_at(b, 4), max: vec3_at(b, 16), layer: u32_at(b, 28) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(out: &mut Vec<u8>, name: &str, records: &[Vec<u8>]) {
        out.extend_from_slice(name.as_bytes());
        out.push(b'\n');
        let len: usize = records.iter().map(Vec::len).sum();
        out.extend_from_slice(&(len as u32).to_le_bytes());
        for r in records {
            out.extend_from_slice(r);
        }
        out.push(b'\n');
    }

    fn words(ws: &[u32]) -> Vec<u8> {
        ws.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn sample() -> Vec<u8> {
        let mut data = b"[VERSION]\n46\n".to_vec();
        data.resize(HEADER_LEN, 0);
        data.extend_from_slice(b"<fileDates>\n2740459947 134265985168302590\n1373509686 134280261081509208 7\n</fileDates>\n");
        data.extend_from_slice(b"<module>\n");

        let mut node = words(&[0xAAAA_0001, 0xAAAA_0002, 0x40]);
        for f in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0] {
            node.extend_from_slice(&f.to_le_bytes());
        }
        node.extend_from_slice(&[1, 2, 3, 4]);
        // A newline byte inside the block must not end a line.
        let mut node2 = node.clone();
        node2[0] = b'\n';
        block(&mut data, "fwMapDataStore", &[node, node2]);

        let mut proxy = words(&[7, 0, 3, 0xBEEF, 0xAAAA_0001]);
        for f in [-583.8f32, -1059.2, 23.2, 0.0, 0.0, 0.7071, 0.7071, -10.0, -10.0, -5.0, 10.0, 10.0, 5.0] {
            proxy.extend_from_slice(&f.to_le_bytes());
        }
        for w in [1u64, 2, 3, 4] {
            proxy.extend_from_slice(&w.to_le_bytes());
        }
        block(&mut data, "CInteriorProxy", &[proxy]);

        let mut bound = words(&[0xC0DE]);
        for f in [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0] {
            bound.extend_from_slice(&f.to_le_bytes());
        }
        bound.extend_from_slice(&9u32.to_le_bytes());
        block(&mut data, "BoundsStore", &[bound]);

        data.extend_from_slice(b"</module>\n\0");
        data
    }

    #[test]
    fn reads_every_block_and_the_dates() {
        let cache = parse_cache_dat(&sample()).unwrap();
        assert_eq!(cache.version, "46");
        assert_eq!(cache.file_dates.len(), 2);
        assert_eq!(cache.file_dates[1], CacheFileDate { file_name: 1373509686, timestamp: 134280261081509208, file_id: 7 });

        assert_eq!(cache.map_nodes.len(), 2);
        let n = cache.map_nodes[0];
        assert_eq!((n.name, n.parent, n.content_flags), (0xAAAA_0001, 0xAAAA_0002, 0x40));
        assert_eq!(n.streaming_min, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(n.entities_max, Vec3::new(10.0, 11.0, 12.0));
        assert_eq!(n.flags, [1, 2, 3, 4]);

        assert_eq!(cache.interior_proxies.len(), 1);
        let p = cache.interior_proxies[0];
        assert_eq!((p.name, p.parent), (0xBEEF, 0xAAAA_0001));
        assert_eq!(p.unknown, [7, 0, 3]);
        assert_eq!(p.position, Vec3::new(-583.8, -1059.2, 23.2));
        assert_eq!(p.bb_max, Vec3::new(10.0, 10.0, 5.0));
        assert_eq!(p.trailing, [1, 2, 3, 4]);

        assert_eq!(cache.bounds, vec![BoundsStoreItem { name: 0xC0DE, min: Vec3::new(0.0, 1.0, 2.0), max: Vec3::new(3.0, 4.0, 5.0), layer: 9 }]);
    }

    #[test]
    fn a_block_running_past_the_end_is_an_error() {
        let mut data = sample();
        let at = data.windows(15).position(|w| w == b"CInteriorProxy\n").unwrap() + 15;
        data[at..at + 4].copy_from_slice(&100_000u32.to_le_bytes());
        assert!(parse_cache_dat(&data).is_err());
    }

    #[test]
    fn a_file_without_the_header_is_an_error() {
        assert!(parse_cache_dat(b"not a cache file").is_err());
    }
}
