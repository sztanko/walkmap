//! Minimal PMTiles v3 writer — enough for our bespoke buildings archives.
//! Root-directory-only layout (readers accept large root dirs; the 16KB
//! figure in the spec is a recommendation for CDN efficiency, and these
//! archives are merged by tile-join immediately anyway).

use anyhow::Result;
use std::io::Write;

fn varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            break;
        }
        buf.push(b | 0x80);
    }
}

/// PMTiles tile id: cumulative count of tiles below zoom z + Hilbert index.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let base: u64 = (0..z).map(|i| 1u64 << (2 * i)).sum();
    let n = 1u32 << z;
    let (mut rx, mut ry): (u32, u32);
    let (mut hx, mut hy) = (x, y);
    let mut d: u64 = 0;
    let mut s = n / 2;
    while s > 0 {
        rx = if (hx & s) > 0 { 1 } else { 0 };
        ry = if (hy & s) > 0 { 1 } else { 0 };
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        // rotate
        if ry == 0 {
            if rx == 1 {
                hx = s.wrapping_sub(1).wrapping_sub(hx) & (n - 1);
                hy = s.wrapping_sub(1).wrapping_sub(hy) & (n - 1);
            }
            std::mem::swap(&mut hx, &mut hy);
        }
        s /= 2;
    }
    base + d
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// tiles: (tile_id, gzipped MVT bytes), any order. bounds [w,s,e,n].
pub fn write(
    path: &std::path::Path,
    mut tiles: Vec<(u64, Vec<u8>)>,
    metadata_json: &str,
    min_zoom: u8,
    max_zoom: u8,
    bounds: [f64; 4],
) -> Result<()> {
    tiles.sort_by_key(|(id, _)| *id);

    // clustered tile data + directory entries
    let mut dir_ids = Vec::with_capacity(tiles.len());
    let mut dir_offsets = Vec::with_capacity(tiles.len());
    let mut dir_lengths = Vec::with_capacity(tiles.len());
    let mut data: Vec<u8> = Vec::new();
    for (id, bytes) in &tiles {
        dir_ids.push(*id);
        dir_offsets.push(data.len() as u64);
        dir_lengths.push(bytes.len() as u64);
        data.extend_from_slice(bytes);
    }

    // directory encoding: n, tile_id deltas, run lengths (all 1), lengths, offsets
    let mut dir = Vec::new();
    varint(&mut dir, dir_ids.len() as u64);
    let mut last = 0u64;
    for id in &dir_ids {
        varint(&mut dir, id - last);
        last = *id;
    }
    for _ in &dir_ids {
        varint(&mut dir, 1); // run length
    }
    for l in &dir_lengths {
        varint(&mut dir, *l);
    }
    for (i, o) in dir_offsets.iter().enumerate() {
        // 0 = "directly follows previous entry"; else offset+1
        if i > 0 && *o == dir_offsets[i - 1] + dir_lengths[i - 1] {
            varint(&mut dir, 0);
        } else {
            varint(&mut dir, o + 1);
        }
    }
    let root_dir = gzip(&dir);
    let metadata = gzip(metadata_json.as_bytes());

    const HDR: u64 = 127;
    let root_off = HDR;
    let meta_off = root_off + root_dir.len() as u64;
    let data_off = meta_off + metadata.len() as u64;

    let mut h = Vec::with_capacity(127);
    h.extend_from_slice(b"PMTiles");
    h.push(3u8);
    for v in [
        root_off,
        root_dir.len() as u64,
        meta_off,
        metadata.len() as u64,
        0, // leaf dirs offset
        0, // leaf dirs length
        data_off,
        data.len() as u64,
        tiles.len() as u64, // addressed tiles
        tiles.len() as u64, // tile entries
        tiles.len() as u64, // tile contents
    ] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    h.push(1); // clustered
    h.push(2); // internal compression: gzip
    h.push(2); // tile compression: gzip
    h.push(1); // tile type: mvt
    h.push(min_zoom);
    h.push(max_zoom);
    for v in [bounds[0], bounds[1], bounds[2], bounds[3]] {
        h.extend_from_slice(&((v * 1e7) as i32).to_le_bytes());
    }
    h.push(min_zoom); // center zoom
    for v in [(bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0] {
        h.extend_from_slice(&((v * 1e7) as i32).to_le_bytes());
    }
    assert_eq!(h.len(), 127, "PMTiles header must be 127 bytes");

    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    out.write_all(&h)?;
    out.write_all(&root_dir)?;
    out.write_all(&metadata)?;
    out.write_all(&data)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_match_spec_examples() {
        // known values from the PMTiles spec/implementations
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
    }
}
