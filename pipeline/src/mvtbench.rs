//! EXPERIMENT: how fast could a bespoke MVT encoder tile our buildings,
//! compared with tippecanoe on identical input?
//!
//! Encodes the buildings layer for z12–z15 straight from memory: web-mercator
//! projection → per-tile bucketing → Sutherland–Hodgman clipping → MVT
//! protobuf → gzip, rayon-parallel over tiles. Deliberately NOT production:
//! no feature dropping/coalescing, no tile-size enforcement, no PMTiles
//! container (bytes are counted, not written). Numbers are indicative.

use crate::osm::Building;
use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::io::Write;
use std::time::Instant;

const EXTENT: f64 = 4096.0;
const BUFFER: f64 = 128.0; // tile-edge clip buffer in extent units

// ---- minimal protobuf/MVT writer ----

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

/// polygon rings (already in tile coords) → MVT geometry commands
fn encode_geometry(rings: &[Vec<(i32, i32)>]) -> Vec<u8> {
    let mut g = Vec::with_capacity(rings.iter().map(|r| r.len() * 2 + 4).sum());
    let (mut cx, mut cy) = (0i64, 0i64);
    let mut cmds: Vec<u32> = Vec::new();
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        cmds.push((1 << 3) | 1); // MoveTo, count 1
        let (x0, y0) = (ring[0].0 as i64, ring[0].1 as i64);
        cmds.push(zigzag(x0 - cx) as u32);
        cmds.push(zigzag(y0 - cy) as u32);
        cx = x0;
        cy = y0;
        let n = ring.len() - 1; // skip closing duplicate
        cmds.push((((n - 1) as u32) << 3) | 2); // LineTo
        for p in &ring[1..n] {
            let (x, y) = (p.0 as i64, p.1 as i64);
            cmds.push(zigzag(x - cx) as u32);
            cmds.push(zigzag(y - cy) as u32);
            cx = x;
            cy = y;
        }
        cmds.push((1 << 3) | 7); // ClosePath
    }
    for c in cmds {
        varint(&mut g, c as u64);
    }
    g
}

fn encode_tile(features: &[Vec<Vec<(i32, i32)>>]) -> Vec<u8> {
    // one shared dummy attribute set: pid=0,t=100,c="#4e79a7" — attribute
    // encoding cost is negligible next to geometry, keys/values are interned
    let mut layer = Vec::with_capacity(features.len() * 64);
    tag(&mut layer, 15, 0);
    varint(&mut layer, 2); // version
    bytes_field(&mut layer, 1, b"buildings");
    tag(&mut layer, 5, 0);
    varint(&mut layer, EXTENT as u64);
    for k in ["pid", "t", "c"] {
        bytes_field(&mut layer, 3, k.as_bytes());
    }
    // values: uint 0, uint 100, string
    let mut v0 = Vec::new();
    tag(&mut v0, 5, 0);
    varint(&mut v0, 0);
    let mut v1 = Vec::new();
    tag(&mut v1, 5, 0);
    varint(&mut v1, 100);
    let mut v2 = Vec::new();
    bytes_field(&mut v2, 1, b"#4e79a7");
    for v in [&v0, &v1, &v2] {
        bytes_field(&mut layer, 4, v);
    }
    for rings in features {
        let geom = encode_geometry(rings);
        if geom.is_empty() {
            continue;
        }
        let mut f = Vec::with_capacity(geom.len() + 16);
        let mut tags_buf = Vec::new();
        for (k, v) in [(0u64, 0u64), (1, 1), (2, 2)] {
            varint(&mut tags_buf, k);
            varint(&mut tags_buf, v);
        }
        bytes_field(&mut f, 2, &tags_buf);
        tag(&mut f, 3, 0);
        varint(&mut f, 3); // POLYGON
        bytes_field(&mut f, 4, &geom);
        bytes_field(&mut layer, 2, &f);
    }
    let mut tile = Vec::with_capacity(layer.len() + 8);
    bytes_field(&mut tile, 3, &layer);
    tile
}

// ---- clipping (Sutherland–Hodgman against an axis-aligned box) ----

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

// ---- the benchmark ----

pub fn bench(buildings: &[Building]) -> Result<()> {
    eprintln!("mvt-bench: {} buildings", buildings.len());
    // project once to unit web mercator
    let t0 = Instant::now();
    let projected: Vec<Vec<Vec<(f64, f64)>>> = buildings
        .par_iter()
        .map(|b| {
            b.rings
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|p| {
                            let x = (p[0] + 180.0) / 360.0;
                            let s = (p[1].to_radians()).sin().clamp(-0.9999, 0.9999);
                            let y = 0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI);
                            (x, y)
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    eprintln!("  project: {:.2}s", t0.elapsed().as_secs_f64());

    let mut grand_total = 0.0;
    for z in 12u32..=15 {
        let tz = Instant::now();
        let scale = (1u64 << z) as f64;
        // bucket features by tile (bbox over outer ring)
        let mut buckets: FxHashMap<(u32, u32), Vec<usize>> = FxHashMap::default();
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
            let (tx0, ty0) = (((x0 - pad) * scale) as u32, ((y0 - pad) * scale) as u32);
            let (tx1, ty1) = (((x1 + pad) * scale) as u32, ((y1 + pad) * scale) as u32);
            for tx in tx0..=tx1 {
                for ty in ty0..=ty1 {
                    buckets.entry((tx, ty)).or_default().push(i);
                }
            }
        }
        let tiles: Vec<((u32, u32), Vec<usize>)> = buckets.into_iter().collect();
        let (n_tiles, bytes): (usize, u64) = tiles
            .par_iter()
            .map(|((tx, ty), idxs)| {
                let feats: Vec<Vec<Vec<(i32, i32)>>> = idxs
                    .iter()
                    .filter_map(|&i| {
                        let rings: Vec<Vec<(i32, i32)>> = projected[i]
                            .iter()
                            .filter_map(|ring| {
                                let tile_coords: Vec<(f64, f64)> = ring
                                    .iter()
                                    .map(|&(x, y)| {
                                        (x * scale * EXTENT - *tx as f64 * EXTENT,
                                         y * scale * EXTENT - *ty as f64 * EXTENT)
                                    })
                                    .collect();
                                let clipped = clip_ring(&tile_coords, -BUFFER, EXTENT + BUFFER);
                                if clipped.len() < 3 {
                                    return None;
                                }
                                let mut q: Vec<(i32, i32)> = clipped
                                    .iter()
                                    .map(|&(x, y)| (x.round() as i32, y.round() as i32))
                                    .collect();
                                q.dedup();
                                if q.first() != q.last() {
                                    if let Some(&f) = q.first() {
                                        q.push(f);
                                    }
                                }
                                (q.len() >= 4).then_some(q)
                            })
                            .collect();
                        (!rings.is_empty()).then_some(rings)
                    })
                    .collect();
                if feats.is_empty() {
                    return (0usize, 0u64);
                }
                let raw = encode_tile(&feats);
                let mut enc =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
                enc.write_all(&raw).unwrap();
                (1usize, enc.finish().unwrap().len() as u64)
            })
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        let dt = tz.elapsed().as_secs_f64();
        grand_total += dt;
        eprintln!(
            "  z{z}: {n_tiles} tiles, {:.1} MB gz, {:.2}s",
            bytes as f64 / 1e6,
            dt
        );
    }
    eprintln!("mvt-bench encode z12-15: {:.1}s", grand_total);
    eprintln!("mvt-bench total incl. projection: {:.1}s", t0.elapsed().as_secs_f64());
    Ok(())
}
