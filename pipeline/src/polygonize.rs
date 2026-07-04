//! Trace label-grid region boundaries into polygons (GDALPolygonize-style).
//!
//! For every cell whose 4-neighbor has a different label we emit that cell
//! side as an oriented edge (interior on the RIGHT, walking the cell boundary
//! TL→TR→BR→BL in y-down grid coordinates). Per label, those edges stitch
//! into closed rings; positive shoelace area (in y-down coords) = shell,
//! negative = hole. Saddle corners are resolved by preferring the sharpest
//! right turn, which keeps diagonally-touching regions as separate rings.

use rayon::prelude::*;
use rustc_hash::FxHashMap;

pub const NODATA: u32 = u32::MAX;

/// One label's geometry: list of polygons, each polygon = [shell, holes...],
/// ring = closed list of grid-corner (x, y) points.
pub struct LabelPolys {
    pub label: u32,
    pub polys: Vec<Vec<Vec<[f64; 2]>>>,
}

pub fn polygonize(labels: &[u32], w: usize, h: usize) -> Vec<LabelPolys> {
    assert_eq!(labels.len(), w * h);
    let cw = w + 1; // corner-grid width
    let corner = |x: usize, y: usize| -> u64 { (y * cw + x) as u64 };
    let at = |x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            NODATA
        } else {
            labels[y as usize * w + x as usize]
        }
    };

    // collect oriented boundary edges per label
    let mut edges: FxHashMap<u32, Vec<(u64, u64)>> = FxHashMap::default();
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l == NODATA {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            if at(xi, yi - 1) != l {
                edges.entry(l).or_default().push((corner(x, y), corner(x + 1, y)));
            }
            if at(xi + 1, yi) != l {
                edges.entry(l).or_default().push((corner(x + 1, y), corner(x + 1, y + 1)));
            }
            if at(xi, yi + 1) != l {
                edges.entry(l).or_default().push((corner(x + 1, y + 1), corner(x, y + 1)));
            }
            if at(xi - 1, yi) != l {
                edges.entry(l).or_default().push((corner(x, y + 1), corner(x, y)));
            }
        }
    }

    let cw64 = cw as i64;
    let mut out: Vec<LabelPolys> = edges
        .into_par_iter()
        .map(|(label, es)| LabelPolys { label, polys: stitch(es, cw64) })
        .collect();
    out.sort_by_key(|lp| lp.label);
    out
}

fn dir(from: u64, to: u64, cw: i64) -> (i64, i64) {
    let (fx, fy) = ((from as i64) % cw, (from as i64) / cw);
    let (tx, ty) = ((to as i64) % cw, (to as i64) / cw);
    ((tx - fx).signum(), (ty - fy).signum())
}

/// Stitch oriented unit edges into rings, then assemble shells + holes.
fn stitch(es: Vec<(u64, u64)>, cw: i64) -> Vec<Vec<Vec<[f64; 2]>>> {
    // start corner -> outgoing edge indices (≤2 per corner)
    let mut adj: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    for (i, &(a, _)) in es.iter().enumerate() {
        adj.entry(a).or_default().push(i as u32);
    }
    let mut used = vec![false; es.len()];
    let mut rings: Vec<Vec<u64>> = Vec::new();

    for start in 0..es.len() {
        if used[start] {
            continue;
        }
        let mut ring: Vec<u64> = Vec::new();
        let mut cur = start;
        loop {
            used[cur] = true;
            let (a, b) = es[cur];
            ring.push(a);
            let d_in = dir(a, b, cw);
            // pick the next unused edge out of b, preferring the sharpest right
            // turn (max cross product in y-down coords); never straight back.
            let Some(cands) = adj.get(&b) else { break };
            let mut best: Option<(i64, u32)> = None;
            for &ei in cands.iter() {
                if used[ei as usize] {
                    continue;
                }
                let (ea, eb) = es[ei as usize];
                let d_out = dir(ea, eb, cw);
                if d_out == (-d_in.0, -d_in.1) {
                    continue; // U-turn
                }
                let cross = d_in.0 * d_out.1 - d_in.1 * d_out.0;
                if best.map_or(true, |(c, _)| cross > c) {
                    best = Some((cross, ei));
                }
            }
            match best {
                Some((_, ei)) => cur = ei as usize,
                None => break, // ring closed (back at start corner)
            }
            if cur == start {
                break;
            }
        }
        if ring.len() >= 4 {
            rings.push(ring);
        }
    }

    // corners -> (x, y) points, dropping collinear runs
    let mut shells: Vec<(f64, Vec<[f64; 2]>)> = Vec::new();
    let mut holes: Vec<Vec<[f64; 2]>> = Vec::new();
    for ring in rings {
        let pts = simplify_ring(&ring, cw);
        if pts.len() < 4 {
            continue;
        }
        let area = shoelace(&pts);
        if area > 0.0 {
            shells.push((area, pts));
        } else if area < 0.0 {
            holes.push(pts);
        }
    }

    // assign each hole to the smallest shell containing a point just inside
    // the shell region (offset from the hole boundary toward its right side)
    shells.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mut polys: Vec<Vec<Vec<[f64; 2]>>> = shells.iter().map(|(_, s)| vec![s.clone()]).collect();
    'holes: for hole in holes {
        let p = inside_right_of(&hole);
        for (i, (_, shell)) in shells.iter().enumerate() {
            if point_in_ring(p, shell) {
                polys[i].push(hole);
                continue 'holes;
            }
        }
        // pathological: hole outside every shell — drop it
    }
    polys
}

fn simplify_ring(ring: &[u64], cw: i64) -> Vec<[f64; 2]> {
    let pts: Vec<[f64; 2]> = ring
        .iter()
        .map(|&c| [((c as i64) % cw) as f64, ((c as i64) / cw) as f64])
        .collect();
    let n = pts.len();
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(n / 2);
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let cur = pts[i];
        let next = pts[(i + 1) % n];
        let cross = (cur[0] - prev[0]) * (next[1] - cur[1]) - (cur[1] - prev[1]) * (next[0] - cur[0]);
        if cross.abs() > 1e-9 {
            out.push(cur);
        }
    }
    if let Some(&first) = out.first() {
        out.push(first); // close
    }
    out
}

fn shoelace(pts: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..pts.len() - 1 {
        a += pts[i][0] * pts[i + 1][1] - pts[i + 1][0] * pts[i][1];
    }
    a / 2.0
}

/// A point just inside the region lying to the RIGHT of the hole ring's first edge.
fn inside_right_of(ring: &[[f64; 2]]) -> [f64; 2] {
    let (a, b) = (ring[0], ring[1]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let (ux, uy) = (dx / len, dy / len);
    // right normal in y-down coords
    let (nx, ny) = (-uy, ux);
    [(a[0] + b[0]) / 2.0 + 0.5 * nx, (a[1] + b[1]) / 2.0 + 0.5 * ny]
}

fn point_in_ring(p: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    for i in 0..ring.len() - 1 {
        let (a, b) = (ring[i], ring[i + 1]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x > p[0] {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total_cells(polys: &[Vec<Vec<[f64; 2]>>]) -> f64 {
        polys
            .iter()
            .map(|rings| {
                let shell = shoelace(&rings[0]);
                let holes: f64 = rings[1..].iter().map(|r| shoelace(r)).sum();
                shell + holes // holes are negative
            })
            .sum()
    }

    #[test]
    fn single_cell() {
        let polys = polygonize(&[7], 1, 1);
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].label, 7);
        assert_eq!(polys[0].polys.len(), 1);
        assert!((total_cells(&polys[0].polys) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn two_regions_split() {
        // 4x1: A A B B
        let polys = polygonize(&[0, 0, 1, 1], 4, 1);
        assert_eq!(polys.len(), 2);
        for lp in &polys {
            assert!((total_cells(&lp.polys) - 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn ring_with_hole() {
        // 3x3 of A with B in the middle → A must be a polygon with a hole
        #[rustfmt::skip]
        let g = [
            0, 0, 0,
            0, 1, 0,
            0, 0, 0,
        ];
        let polys = polygonize(&g, 3, 3);
        let a = polys.iter().find(|p| p.label == 0).unwrap();
        assert_eq!(a.polys.len(), 1);
        assert_eq!(a.polys[0].len(), 2, "shell + hole");
        assert!((total_cells(&a.polys) - 8.0).abs() < 1e-9);
        let b = polys.iter().find(|p| p.label == 1).unwrap();
        assert!((total_cells(&b.polys) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nodata_is_a_gap() {
        let g = [0, NODATA, 0];
        let polys = polygonize(&g, 3, 1);
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].polys.len(), 2, "two disjoint squares");
        assert!((total_cells(&polys[0].polys) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn checkerboard_saddle() {
        // 2x2 checkerboard: each label = two diagonal cells touching at the
        // center corner; right-turn preference keeps them as separate rings.
        #[rustfmt::skip]
        let g = [
            0, 1,
            1, 0,
        ];
        let polys = polygonize(&g, 2, 2);
        for lp in &polys {
            assert_eq!(lp.polys.len(), 2, "label {} should be two separate squares", lp.label);
            assert!((total_cells(&lp.polys) - 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn enclave_inside_hole() {
        // A ring, B hole containing a C enclave: 5x5
        #[rustfmt::skip]
        let g = [
            0, 0, 0, 0, 0,
            0, 1, 1, 1, 0,
            0, 1, 2, 1, 0,
            0, 1, 1, 1, 0,
            0, 0, 0, 0, 0,
        ];
        let polys = polygonize(&g, 5, 5);
        let a = polys.iter().find(|p| p.label == 0).unwrap();
        assert!((total_cells(&a.polys) - 16.0).abs() < 1e-9);
        let b = polys.iter().find(|p| p.label == 1).unwrap();
        assert_eq!(b.polys[0].len(), 2, "B has a hole for C");
        assert!((total_cells(&b.polys) - 8.0).abs() < 1e-9);
        let c = polys.iter().find(|p| p.label == 2).unwrap();
        assert!((total_cells(&c.polys) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn areas_partition_the_grid() {
        // pseudo-random 40x30 grid with 5 labels + nodata: total polygon area
        // (shells minus holes) must equal the number of labeled cells.
        let (w, h) = (40usize, 30usize);
        let mut labels = vec![0u32; w * h];
        let mut seed = 12345u64;
        let mut labeled = 0usize;
        for c in labels.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let v = (seed >> 33) % 6;
            *c = if v == 5 { NODATA } else { labeled += 1; v as u32 };
        }
        let polys = polygonize(&labels, w, h);
        let total: f64 = polys.iter().map(|lp| total_cells(&lp.polys)).sum();
        assert!(
            (total - labeled as f64).abs() < 1e-6,
            "total {} != labeled {}",
            total,
            labeled
        );
    }
}
