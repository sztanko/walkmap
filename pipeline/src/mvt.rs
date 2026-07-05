//! Production bespoke MVT encoder for the BUILDINGS layer (z12–15).
//!
//! Why not tippecanoe here: buildings dominate tiling cost (~95%) and their
//! geometry is identical across every feature type — so we clip, quantize and
//! command-encode each tile's geometry ONCE per city, then stamp the per-type
//! attributes (pid, t, c) over the cached buffers. Tippecanoe still tiles the
//! partitions layer (its shared-border simplification is worth keeping);
//! tile-join merges the two (see tiles.rs).
//!
//! Size management: buildings are ranked by footprint area (descending,
//! computed once); each tile keeps the largest-first prefix whose estimated
//! raw size fits a budget — the same drop decision for every type, which is
//! what makes geometry sharing sound.

use crate::osm::Building;
use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXTENT: f64 = 4096.0;
const BUFFER: f64 = 8.0;
const MIN_Z: u8 = 12;
const MAX_Z: u8 = 15;
/// raw (pre-gzip) per-tile geometry budget; ~500KB raw ≈ 150-250KB gz
const TILE_BUDGET: usize = 500_000;
/// drop features whose quantized outer ring is sub-pixel noise (the bespoke
/// analogue of tippecanoe's tiny-polygon reduction; bites only at z12–13)
const MIN_QUANT_AREA: i64 = 12;

pub struct TypeAttrs {
    pub id: String,
    /// per building: None = not in this type's output
    pub pid_t: Vec<Option<(u32, Option<u32>)>>,
    pub colors: Vec<u8>,
}

// ---- protobuf helpers ----

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
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn tag(buf: &mut Vec<u8>, field: u64, wire: u64) {
    varint(buf, (field << 3) | wire);
}
fn bytes_field(buf: &mut Vec<u8>, field: u64, data: &[u8]) {
    tag(buf, field, 2);
    varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

fn encode_geometry(rings: &[Vec<(i32, i32)>]) -> Vec<u8> {
    let mut cmds: Vec<u32> = Vec::with_capacity(rings.iter().map(|r| r.len() * 2 + 3).sum());
    let (mut cx, mut cy) = (0i64, 0i64);
    for ring in rings {
        if ring.len() < 4 {
            continue;
        }
        cmds.push((1 << 3) | 1);
        let (x0, y0) = (ring[0].0 as i64, ring[0].1 as i64);
        cmds.push(zigzag(x0 - cx) as u32);
        cmds.push(zigzag(y0 - cy) as u32);
        cx = x0;
        cy = y0;
        let n = ring.len() - 1;
        cmds.push((((n - 1) as u32) << 3) | 2);
        for p in &ring[1..n] {
            cmds.push(zigzag(p.0 as i64 - cx) as u32);
            cmds.push(zigzag(p.1 as i64 - cy) as u32);
            cx = p.0 as i64;
            cy = p.1 as i64;
        }
        cmds.push((1 << 3) | 7);
    }
    let mut g = Vec::with_capacity(cmds.len() * 2);
    for c in cmds {
        varint(&mut g, c as u64);
    }
    g
}

fn clip_ring(ring: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let mut pts = ring.to_vec();
    for axis in 0..2 {
        for (bound, keep_ge) in [(lo, true), (hi, false)] {
            if pts.len() < 3 {
                return Vec::new();
            }
            let inside = |p: (f64, f64)| {
                let v = if axis == 0 { p.0 } else { p.1 };
                if keep_ge {
                    v >= bound
                } else {
                    v <= bound
                }
            };
            let mut out = Vec::with_capacity(pts.len() + 4);
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                let (ia, ib) = (inside(a), inside(b));
                if ia {
                    out.push(a);
                }
                if ia != ib {
                    let t = if axis == 0 {
                        (bound - a.0) / (b.0 - a.0)
                    } else {
                        (bound - a.1) / (b.1 - a.1)
                    };
                    out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
                }
            }
            pts = out;
        }
    }
    pts
}

/// MVT v2: exterior rings positive shoelace in tile coords, holes negative
fn oriented(mut ring: Vec<(i32, i32)>, want_positive: bool) -> Vec<(i32, i32)> {
    let mut area = 0i64;
    for i in 0..ring.len() - 1 {
        area += ring[i].0 as i64 * ring[i + 1].1 as i64 - ring[i + 1].0 as i64 * ring[i].1 as i64;
    }
    if (area > 0) != want_positive {
        ring.reverse();
    }
    ring
}

struct CachedFeature {
    bld: u32,
    geom: Vec<u8>,
}
struct CachedTile {
    z: u8,
    x: u32,
    y: u32,
    feats: Vec<CachedFeature>,
}

fn quantize(ring: &[(f64, f64)]) -> Option<Vec<(i32, i32)>> {
    let mut q: Vec<(i32, i32)> = ring.iter().map(|&(x, y)| (x.round() as i32, y.round() as i32)).collect();
    q.dedup();
    if q.first() != q.last() {
        if let Some(&f) = q.first() {
            q.push(f);
        }
    }
    (q.len() >= 4).then_some(q)
}

/// Build one archive per feature type, sharing clipped geometry across types.
pub fn build_buildings_archives(
    buildings: &[Building],
    types: &[TypeAttrs],
    out_dir: &Path,
    bounds: [f64; 4],
) -> Result<Vec<(String, PathBuf)>> {
    // project to unit mercator + rank by footprint area
    let projected: Vec<Vec<Vec<(f64, f64)>>> = buildings
        .par_iter()
        .map(|b| {
            b.rings
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|p| {
                            let x = (p[0] + 180.0) / 360.0;
                            let s = p[1].to_radians().sin().clamp(-0.9999, 0.9999);
                            let y = 0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI);
                            (x, y)
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let area: Vec<f64> = projected
        .par_iter()
        .map(|rings| {
            let r = &rings[0];
            let mut a = 0.0;
            for i in 0..r.len() - 1 {
                a += r[i].0 * r[i + 1].1 - r[i + 1].0 * r[i].1;
            }
            a.abs()
        })
        .collect();

    // shared clipped/encoded geometry per tile, largest-first within budget
    let mut tiles: Vec<CachedTile> = Vec::new();
    for z in MIN_Z..=MAX_Z {
        let scale = (1u64 << z) as f64;
        let mut buckets: FxHashMap<(u32, u32), Vec<u32>> = FxHashMap::default();
        for (i, rings) in projected.iter().enumerate() {
            let outer = &rings[0];
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for &(x, y) in outer {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            let pad = BUFFER / EXTENT / scale;
            for tx in ((x0 - pad) * scale) as u32..=((x1 + pad) * scale) as u32 {
                for ty in ((y0 - pad) * scale) as u32..=((y1 + pad) * scale) as u32 {
                    buckets.entry((tx, ty)).or_default().push(i as u32);
                }
            }
        }
        let mut zt: Vec<CachedTile> = buckets
            .into_par_iter()
            .map(|((tx, ty), mut idxs)| {
                idxs.sort_unstable_by(|&a, &b| {
                    area[b as usize].partial_cmp(&area[a as usize]).unwrap()
                });
                let mut feats = Vec::with_capacity(idxs.len());
                let mut used = 0usize;
                for &i in &idxs {
                    if used > TILE_BUDGET {
                        break; // smallest buildings dropped first
                    }
                    let rings: Vec<Vec<(i32, i32)>> = projected[i as usize]
                        .iter()
                        .enumerate()
                        .filter_map(|(ri, ring)| {
                            let tc: Vec<(f64, f64)> = ring
                                .iter()
                                .map(|&(x, y)| {
                                    (x * scale * EXTENT - tx as f64 * EXTENT,
                                     y * scale * EXTENT - ty as f64 * EXTENT)
                                })
                                .collect();
                            let clipped = clip_ring(&tc, -BUFFER, EXTENT + BUFFER);
                            if clipped.len() < 3 {
                                return None;
                            }
                            quantize(&clipped).map(|q| oriented(q, ri == 0))
                        })
                        .collect();
                    if rings.is_empty() || rings[0].len() < 4 {
                        continue;
                    }
                    let mut a2 = 0i64;
                    let outer = &rings[0];
                    for k in 0..outer.len() - 1 {
                        a2 += outer[k].0 as i64 * outer[k + 1].1 as i64
                            - outer[k + 1].0 as i64 * outer[k].1 as i64;
                    }
                    if a2.abs() < 2 * MIN_QUANT_AREA {
                        continue; // sub-pixel speck at this zoom
                    }
                    let geom = encode_geometry(&rings);
                    if geom.is_empty() {
                        continue;
                    }
                    used += geom.len() + 16;
                    feats.push(CachedFeature { bld: i, geom });
                }
                CachedTile { z, x: tx, y: ty, feats }
            })
            .filter(|t| !t.feats.is_empty())
            .collect();
        tiles.append(&mut zt);
    }
    eprintln!("  mvt: shared geometry for {} tiles cached", tiles.len());

    // per type: stamp attributes over the cached geometry, gzip, write archive
    let mut out = Vec::with_capacity(types.len());
    for t in types {
        let encoded: Vec<(u64, Vec<u8>)> = tiles
            .par_iter()
            .filter_map(|tile| {
                let mut values: Vec<Vec<u8>> = Vec::new();
                let mut val_idx: FxHashMap<(u8, u64, String), u32> = FxHashMap::default();
                let mut feats_buf = Vec::new();
                let mut any = false;
                for f in &tile.feats {
                    let Some((pid, tsec)) = t.pid_t[f.bld as usize] else { continue };
                    any = true;
                    let mut intern = |kind: u8, num: u64, s: String| -> u32 {
                        *val_idx.entry((kind, num, s.clone())).or_insert_with(|| {
                            let mut v = Vec::new();
                            if kind == 0 {
                                // int_value (field 4): tile-join keeps it numeric
                                // (uint_value gets coerced to string)
                                tag(&mut v, 4, 0);
                                varint(&mut v, num);
                            } else {
                                bytes_field(&mut v, 1, s.as_bytes());
                            }
                            values.push(v);
                            (values.len() - 1) as u32
                        })
                    };
                    let pid_v = intern(0, pid as u64, String::new());
                    let t_v = tsec.map(|s| intern(0, s as u64, String::new()));
                    let c_v = intern(
                        1,
                        0,
                        crate::output::building_color(t.colors[pid as usize], tsec),
                    );
                    let mut tags_buf = Vec::new();
                    varint(&mut tags_buf, 0);
                    varint(&mut tags_buf, pid_v as u64);
                    if let Some(tv) = t_v {
                        varint(&mut tags_buf, 1);
                        varint(&mut tags_buf, tv as u64);
                    }
                    varint(&mut tags_buf, 2);
                    varint(&mut tags_buf, c_v as u64);
                    let mut fb = Vec::with_capacity(f.geom.len() + tags_buf.len() + 12);
                    bytes_field(&mut fb, 2, &tags_buf);
                    tag(&mut fb, 3, 0);
                    varint(&mut fb, 3); // POLYGON
                    bytes_field(&mut fb, 4, &f.geom);
                    bytes_field(&mut feats_buf, 2, &fb);
                }
                if !any {
                    return None;
                }
                let mut layer = Vec::with_capacity(feats_buf.len() + 256);
                tag(&mut layer, 15, 0);
                varint(&mut layer, 2);
                bytes_field(&mut layer, 1, b"buildings");
                tag(&mut layer, 5, 0);
                varint(&mut layer, EXTENT as u64);
                layer.append(&mut feats_buf.clone());
                for k in ["pid", "t", "c"] {
                    bytes_field(&mut layer, 3, k.as_bytes());
                }
                for v in &values {
                    bytes_field(&mut layer, 4, v);
                }
                let mut tile_buf = Vec::with_capacity(layer.len() + 8);
                bytes_field(&mut tile_buf, 3, &layer);
                let mut enc =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
                enc.write_all(&tile_buf).unwrap();
                Some((crate::pmt::tile_id(tile.z, tile.x, tile.y), enc.finish().unwrap()))
            })
            .collect();
        let path = out_dir.join(format!("{}.blds.pmtiles", t.id));
        let meta = format!(
            "{{\"vector_layers\":[{{\"id\":\"buildings\",\"fields\":{{\"pid\":\"Number\",\"t\":\"Number\",\"c\":\"String\"}},\"minzoom\":{MIN_Z},\"maxzoom\":{MAX_Z}}}]}}"
        );
        crate::pmt::write(&path, encoded, &meta, MIN_Z, MAX_Z, bounds)?;
        out.push((t.id.clone(), path));
    }
    Ok(out)
}
