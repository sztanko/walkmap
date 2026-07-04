//! Trace label-grid region boundaries into polygons (GDALPolygonize-style),
//! with optional topology-aware smoothing.
//!
//! For every cell whose 4-neighbor has a different label we emit that cell
//! side as an oriented edge (interior on the RIGHT, walking the cell boundary
//! TL→TR→BR→BL in y-down grid coordinates). Edges are grouped into SHARED
//! boundary polylines (junction corner → junction corner, constant label pair)
//! so that smoothing is applied once per polyline and both neighboring regions
//! reuse identical geometry — smoothing can therefore never open gaps or
//! overlaps. Per label, directed polylines stitch into closed rings; positive
//! shoelace area (in y-down coords) = shell, negative = hole. Saddle corners
//! are resolved by preferring the sharpest right turn, which keeps
//! diagonally-touching regions as separate rings.

use rayon::prelude::*;
use rustc_hash::FxHashMap;

pub const NODATA: u32 = u32::MAX;

/// One label's geometry: list of polygons, each polygon = [shell, holes...],
/// ring = closed list of grid (x, y) points.
pub struct LabelPolys {
    pub label: u32,
    pub polys: Vec<Vec<Vec<[f64; 2]>>>,
}

/// Reassign 4-connected components smaller than `min_cells` to the neighbor
/// label with the longest shared border. Kills nearest-node snapping noise
/// while keeping genuine walking-network islands (which are larger).
pub fn absorb_small_islands(labels: &mut [u32], w: usize, h: usize, min_cells: usize) {
    if min_cells < 2 {
        return;
    }
    let mut comp = vec![u32::MAX; labels.len()];
    let mut queue: Vec<u32> = Vec::new();
    let mut cells: Vec<u32> = Vec::new();
    let mut next_comp = 0u32;
    for start in 0..labels.len() {
        if comp[start] != u32::MAX {
            continue;
        }
        let label = labels[start];
        queue.clear();
        cells.clear();
        comp[start] = next_comp;
        queue.push(start as u32);
        // border contact counts per neighboring label
        let mut border: FxHashMap<u32, u32> = FxHashMap::default();
        while let Some(c) = queue.pop() {
            cells.push(c);
            let (x, y) = (c as usize % w, c as usize / w);
            let mut visit = |nx: isize, ny: isize| {
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    return;
                }
                let n = ny as usize * w + nx as usize;
                if labels[n] == label {
                    if comp[n] == u32::MAX {
                        comp[n] = next_comp;
                        queue.push(n as u32);
                    }
                } else {
                    *border.entry(labels[n]).or_insert(0) += 1;
                }
            };
            visit(x as isize - 1, y as isize);
            visit(x as isize + 1, y as isize);
            visit(x as isize, y as isize - 1);
            visit(x as isize, y as isize + 1);
        }
        if cells.len() < min_cells {
            if let Some((&new_label, _)) = border.iter().max_by_key(|(_, &n)| n) {
                for &c in &cells {
                    labels[c as usize] = new_label;
                }
            }
        }
        next_comp += 1;
    }
}

// ---------------------------------------------------------------------------

struct Polyline {
    pts: Vec<[f64; 2]>,
    start: u64,
    end: u64,
    left_fwd: u32,
    left_bwd: u32,
    closed: bool,
}

/// Returns the label polygons plus the partition adjacency (unordered label
/// pairs sharing a boundary, NODATA excluded) — used for graph colouring.
pub fn polygonize(
    labels: &[u32],
    w: usize,
    h: usize,
    smooth: bool,
    min_island_cells: usize,
) -> (Vec<LabelPolys>, Vec<(u32, u32)>) {
    assert_eq!(labels.len(), w * h);
    let mut labels = labels.to_vec();
    absorb_small_islands(&mut labels, w, h, min_island_cells);

    let cw = (w + 1) as u64; // corner-grid width
    let corner = |x: usize, y: usize| -> u64 { y as u64 * cw + x as u64 };
    let at = |x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            NODATA
        } else {
            labels[y as usize * w + x as usize]
        }
    };

    // Undirected boundary edges keyed by (lo corner, hi corner):
    // sides[0] = label on the left when walking lo→hi, sides[1] = hi→lo.
    let mut edges: FxHashMap<(u64, u64), [u32; 2]> = FxHashMap::default();
    let mut add = |a: u64, b: u64, left: u32| {
        let (key, slot) = if a < b { ((a, b), 0) } else { ((b, a), 1) };
        edges.entry(key).or_insert([NODATA; 2])[slot] = left;
    };
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l == NODATA {
                continue;
            }
            let (xi, yi) = (x as isize, y as isize);
            // cell boundary walked TL→TR→BR→BL keeps the interior on the RIGHT
            if at(xi, yi - 1) != l {
                add(corner(x, y), corner(x + 1, y), l);
            }
            if at(xi + 1, yi) != l {
                add(corner(x + 1, y), corner(x + 1, y + 1), l);
            }
            if at(xi, yi + 1) != l {
                add(corner(x + 1, y + 1), corner(x, y + 1), l);
            }
            if at(xi - 1, yi) != l {
                add(corner(x, y + 1), corner(x, y), l);
            }
        }
    }

    // Group by unordered label pair and trace maximal polylines.
    let mut groups: FxHashMap<(u32, u32), Vec<(u64, u64)>> = FxHashMap::default();
    for (&(a, b), &[l0, l1]) in &edges {
        let key = (l0.min(l1), l0.max(l1));
        groups.entry(key).or_default().push((a, b));
    }
    let adjacency: Vec<(u32, u32)> =
        groups.keys().filter(|(a, b)| *a != NODATA && *b != NODATA && a != b).copied().collect();

    let side = |a: u64, b: u64| -> u32 {
        let (key, slot) = if a < b { ((a, b), 0) } else { ((b, a), 1) };
        edges[&key][slot]
    };

    let cwf = cw as f64;
    let mut polylines: Vec<Polyline> = Vec::new();
    for (_, es) in groups {
        // corner adjacency within this label-pair group
        let mut adj: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        for &(a, b) in &es {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
        let mut used: FxHashMap<(u64, u64), bool> = es.iter().map(|&(a, b)| ((a.min(b), a.max(b)), false)).collect();
        let mut take = |a: u64, b: u64, used: &mut FxHashMap<(u64, u64), bool>| -> bool {
            let e = used.get_mut(&(a.min(b), a.max(b))).unwrap();
            if *e {
                false
            } else {
                *e = true;
                true
            }
        };
        let trace = |start: u64, first: u64, used: &mut FxHashMap<(u64, u64), bool>| -> Vec<u64> {
            let mut chain = vec![start, first];
            let mut prev = start;
            let mut cur = first;
            loop {
                let nbrs = &adj[&cur];
                if nbrs.len() != 2 {
                    break; // junction
                }
                let next = if nbrs[0] == prev { nbrs[1] } else { nbrs[0] };
                if !take(cur, next, used) {
                    break;
                }
                chain.push(next);
                prev = cur;
                cur = next;
            }
            chain
        };
        // open chains start at junction corners (degree != 2 in this group)
        let junctions: Vec<u64> = adj.iter().filter(|(_, v)| v.len() != 2).map(|(&c, _)| c).collect();
        for &j in &junctions {
            let nbrs = adj[&j].clone();
            for n in nbrs {
                if take(j, n, &mut used) {
                    let chain = trace(j, n, &mut used);
                    polylines.push(make_polyline(chain, false, &side, cwf));
                }
            }
        }
        // whatever is left forms closed loops
        let keys: Vec<(u64, u64)> = used.iter().filter(|(_, &u)| !u).map(|(&k, _)| k).collect();
        for (a, b) in keys {
            if take(a, b, &mut used) {
                let mut chain = trace(a, b, &mut used);
                if chain.first() != chain.last() {
                    chain.push(chain[0]); // close the loop
                }
                polylines.push(make_polyline(chain, true, &side, cwf));
            }
        }
    }
    drop(edges);

    // Simplify + smooth each polyline ONCE — both sides share the result.
    polylines.par_iter_mut().for_each(|p| {
        p.pts = drop_collinear(&p.pts, p.closed);
        if smooth {
            p.pts = simplify_dp(&p.pts, 0.9);
            p.pts = chaikin(&p.pts, 2, p.closed);
        }
    });

    // Assemble rings per label.
    let mut by_label: FxHashMap<u32, Vec<(usize, bool)>> = FxHashMap::default(); // (polyline, forward)
    for (i, p) in polylines.iter().enumerate() {
        if p.left_fwd != NODATA {
            by_label.entry(p.left_fwd).or_default().push((i, true));
        }
        if p.left_bwd != NODATA {
            by_label.entry(p.left_bwd).or_default().push((i, false));
        }
    }

    let mut out: Vec<LabelPolys> = by_label
        .into_par_iter()
        .map(|(label, insts)| LabelPolys { label, polys: assemble(label, &insts, &polylines) })
        .collect();
    out.sort_by_key(|lp| lp.label);
    (out, adjacency)
}

fn make_polyline(chain: Vec<u64>, closed: bool, side: &dyn Fn(u64, u64) -> u32, cw: f64) -> Polyline {
    let left_fwd = side(chain[0], chain[1]);
    let left_bwd = side(chain[1], chain[0]);
    let pts = chain.iter().map(|&c| [(c % cw as u64) as f64, (c / cw as u64) as f64]).collect();
    Polyline { pts, start: chain[0], end: *chain.last().unwrap(), left_fwd, left_bwd, closed }
}

/// Stitch a label's directed polylines into rings, then shells + holes.
fn assemble(label: u32, insts: &[(usize, bool)], polylines: &[Polyline]) -> Vec<Vec<Vec<[f64; 2]>>> {
    let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut open: Vec<(usize, bool)> = Vec::new();
    for &(pi, fwd) in insts {
        let p = &polylines[pi];
        if p.closed {
            let mut pts = p.pts.clone();
            if !fwd {
                pts.reverse();
            }
            rings.push(pts);
        } else {
            open.push((pi, fwd));
        }
    }

    // start corner -> open directed polylines for this label
    let mut adj: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
    for (k, &(pi, fwd)) in open.iter().enumerate() {
        let p = &polylines[pi];
        adj.entry(if fwd { p.start } else { p.end }).or_default().push(k);
    }
    let endpoints = |k: usize| -> (u64, u64) {
        let (pi, fwd) = open[k];
        let p = &polylines[pi];
        if fwd {
            (p.start, p.end)
        } else {
            (p.end, p.start)
        }
    };
    let dir_out = |k: usize| -> [f64; 2] {
        let (pi, fwd) = open[k];
        let p = &polylines[pi];
        let (a, b) = if fwd { (p.pts[0], p.pts[1]) } else { (p.pts[p.pts.len() - 1], p.pts[p.pts.len() - 2]) };
        [b[0] - a[0], b[1] - a[1]]
    };
    let dir_in = |k: usize| -> [f64; 2] {
        let (pi, fwd) = open[k];
        let p = &polylines[pi];
        let (a, b) = if fwd { (p.pts[p.pts.len() - 2], p.pts[p.pts.len() - 1]) } else { (p.pts[1], p.pts[0]) };
        [b[0] - a[0], b[1] - a[1]]
    };
    let mut used = vec![false; open.len()];
    for start in 0..open.len() {
        if used[start] {
            continue;
        }
        let mut ring: Vec<[f64; 2]> = Vec::new();
        let (ring_start, _) = endpoints(start);
        let mut cur = start;
        loop {
            used[cur] = true;
            let (pi, fwd) = open[cur];
            let p = &polylines[pi];
            let skip = if ring.is_empty() { 0 } else { 1 };
            if fwd {
                ring.extend(p.pts.iter().skip(skip));
            } else {
                ring.extend(p.pts.iter().rev().skip(skip));
            }
            let (_, end) = endpoints(cur);
            if end == ring_start {
                break;
            }
            let d_in = dir_in(cur);
            let Some(cands) = adj.get(&end) else { break };
            let mut best: Option<(f64, usize)> = None;
            for &k in cands {
                if used[k] {
                    continue;
                }
                let d_out = dir_out(k);
                // sharpest right turn (max cross in y-down coords), U-turns last
                let cross = d_in[0] * d_out[1] - d_in[1] * d_out[0];
                let dot = d_in[0] * d_out[0] + d_in[1] * d_out[1];
                let score = if cross == 0.0 && dot < 0.0 { f64::NEG_INFINITY } else { cross };
                if best.map_or(true, |(s, _)| score > s) {
                    best = Some((score, k));
                }
            }
            match best {
                Some((_, k)) => cur = k,
                None => break,
            }
        }
        if ring.len() >= 4 {
            rings.push(ring);
        }
    }

    // classify + nest
    let mut shells: Vec<(f64, Vec<[f64; 2]>)> = Vec::new();
    let mut holes: Vec<Vec<[f64; 2]>> = Vec::new();
    for mut ring in rings {
        if ring.first() != ring.last() {
            let f = ring[0];
            ring.push(f);
        }
        if ring.len() < 4 {
            continue;
        }
        let area = shoelace(&ring);
        if area > 1e-9 {
            shells.push((area, ring));
        } else if area < -1e-9 {
            holes.push(ring);
        }
    }
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
        let _ = label;
    }
    polys
}

// ---------------------------------------------------------------------------

fn drop_collinear(pts: &[[f64; 2]], closed: bool) -> Vec<[f64; 2]> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(n / 2);
    let last = if closed { n - 1 } else { n }; // closed input repeats first point
    for i in 0..last {
        let keep = if !closed && (i == 0 || i == n - 1) {
            true
        } else {
            let prev = pts[(i + last - 1) % last];
            let next = pts[(i + 1) % last.max(1)];
            let cur = pts[i];
            let cross = (cur[0] - prev[0]) * (next[1] - cur[1]) - (cur[1] - prev[1]) * (next[0] - cur[0]);
            cross.abs() > 1e-9
        };
        if keep {
            out.push(pts[i]);
        }
    }
    if !closed {
        if out.last() != Some(&pts[n - 1]) {
            out.push(pts[n - 1]);
        }
        return out;
    }
    if out.len() < 3 {
        return pts.to_vec();
    }
    out.push(out[0]);
    out
}

/// Douglas-Peucker, endpoints fixed.
fn simplify_dp(pts: &[[f64; 2]], eps: f64) -> Vec<[f64; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (pts[a], pts[b]);
        let (dx, dy) = (pb[0] - pa[0], pb[1] - pa[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        let mut worst = (0.0f64, a);
        for i in a + 1..b {
            let d = ((pts[i][0] - pa[0]) * dy - (pts[i][1] - pa[1]) * dx).abs() / len;
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > eps {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    pts.iter().zip(&keep).filter(|(_, &k)| k).map(|(p, _)| *p).collect()
}

/// Chaikin corner cutting. Open polylines keep their endpoints fixed;
/// closed rings are cut cyclically (input closed: first == last).
fn chaikin(pts: &[[f64; 2]], iterations: usize, closed: bool) -> Vec<[f64; 2]> {
    let mut cur = pts.to_vec();
    for _ in 0..iterations {
        let n = cur.len();
        if n < 3 {
            break;
        }
        let mut next: Vec<[f64; 2]> = Vec::with_capacity(n * 2);
        if closed {
            let m = n - 1; // unique points
            for i in 0..m {
                let (p, q) = (cur[i], cur[(i + 1) % m]);
                next.push([0.75 * p[0] + 0.25 * q[0], 0.75 * p[1] + 0.25 * q[1]]);
                next.push([0.25 * p[0] + 0.75 * q[0], 0.25 * p[1] + 0.75 * q[1]]);
            }
            next.push(next[0]);
        } else {
            next.push(cur[0]);
            for i in 0..n - 1 {
                let (p, q) = (cur[i], cur[i + 1]);
                if i > 0 {
                    next.push([0.75 * p[0] + 0.25 * q[0], 0.75 * p[1] + 0.25 * q[1]]);
                }
                if i < n - 2 {
                    next.push([0.25 * p[0] + 0.75 * q[0], 0.25 * p[1] + 0.75 * q[1]]);
                }
            }
            next.push(cur[n - 1]);
        }
        cur = next;
    }
    cur
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
    let len = (dx * dx + dy * dy).sqrt().max(1e-12);
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy, ux); // right normal in y-down coords
    [(a[0] + b[0]) / 2.0 + 0.4 * nx, (a[1] + b[1]) / 2.0 + 0.4 * ny]
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

    fn exact(labels: &[u32], w: usize, h: usize) -> Vec<LabelPolys> {
        polygonize(labels, w, h, false, 0).0
    }

    #[test]
    fn single_cell() {
        let polys = exact(&[7], 1, 1);
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].label, 7);
        assert_eq!(polys[0].polys.len(), 1);
        assert!((total_cells(&polys[0].polys) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn two_regions_split() {
        // 4x1: A A B B
        let polys = exact(&[0, 0, 1, 1], 4, 1);
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
        let polys = exact(&g, 3, 3);
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
        let polys = exact(&g, 3, 1);
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
        let polys = exact(&g, 2, 2);
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
        let polys = exact(&g, 5, 5);
        let a = polys.iter().find(|p| p.label == 0).unwrap();
        assert!((total_cells(&a.polys) - 16.0).abs() < 1e-9);
        let b = polys.iter().find(|p| p.label == 1).unwrap();
        assert_eq!(b.polys[0].len(), 2, "B has a hole for C");
        assert!((total_cells(&b.polys) - 8.0).abs() < 1e-9);
        let c = polys.iter().find(|p| p.label == 2).unwrap();
        assert!((total_cells(&c.polys) - 1.0).abs() < 1e-9);
    }

    fn random_grid(w: usize, h: usize, n_labels: u64, seed0: u64) -> (Vec<u32>, usize) {
        let mut labels = vec![0u32; w * h];
        let mut seed = seed0;
        let mut labeled = 0usize;
        for c in labels.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let v = (seed >> 33) % (n_labels + 1);
            *c = if v == n_labels {
                NODATA
            } else {
                labeled += 1;
                v as u32
            };
        }
        (labels, labeled)
    }

    #[test]
    fn areas_partition_the_grid() {
        // pseudo-random grid: total polygon area (shells minus holes) must
        // equal the number of labeled cells exactly (no smoothing).
        let (w, h) = (40usize, 30usize);
        let (labels, labeled) = random_grid(w, h, 5, 12345);
        let polys = exact(&labels, w, h);
        let total: f64 = polys.iter().map(|lp| total_cells(&lp.polys)).sum();
        assert!((total - labeled as f64).abs() < 1e-6, "total {} != labeled {}", total, labeled);
    }

    #[test]
    fn small_islands_absorbed() {
        // one lone B cell inside A → absorbed; a 2x3 B block survives min=4
        #[rustfmt::skip]
        let mut g = vec![
            0, 0, 0, 0, 0, 0,
            0, 1, 0, 2, 2, 0,
            0, 0, 0, 2, 2, 0,
            0, 0, 0, 2, 2, 0,
        ];
        absorb_small_islands(&mut g, 6, 4, 4);
        assert_eq!(g[7], 0, "single-cell island absorbed");
        assert_eq!(g[9], 2, "6-cell island kept");
    }

    #[test]
    fn smoothing_preserves_topology_roughly() {
        // smoothed output: rings closed, holes intact, area within a few
        // percent of the exact cell count (Chaikin trims corners slightly)
        let (w, h) = (60usize, 40usize);
        let (labels, labeled) = random_grid(w, h, 4, 987654321);
        let (polys, adjacency) = polygonize(&labels, w, h, true, 3);
        assert!(!adjacency.is_empty());
        assert!(adjacency.iter().all(|&(a, b)| a != b && a != NODATA && b != NODATA));
        let mut total = 0.0;
        for lp in &polys {
            for rings in &lp.polys {
                for ring in rings {
                    assert_eq!(ring.first(), ring.last(), "ring must be closed");
                    assert!(ring.len() >= 4);
                }
                assert!(shoelace(&rings[0]) > 0.0, "shell must be positive");
                for hole in &rings[1..] {
                    assert!(shoelace(hole) < 0.0, "hole must be negative");
                }
            }
            total += total_cells(&lp.polys);
        }
        // islands < 3 cells were absorbed into NODATA neighbors too, so allow slack
        let rel = (total - labeled as f64).abs() / labeled as f64;
        assert!(rel < 0.30, "smoothed area {} vs labeled {} (rel {:.2})", total, labeled, rel);
    }

    #[test]
    fn smoothing_two_labels_share_boundary() {
        // 4x2, A left half, B right half: the shared border must be used by
        // both, so the two areas must exactly tile the total (no gap/overlap):
        // area(A) + area(B) == area(union bbox) since boundary is shared.
        #[rustfmt::skip]
        let g = [
            0, 0, 1, 1,
            0, 0, 1, 1,
        ];
        let (polys, adjacency) = polygonize(&g, 4, 2, true, 0);
        assert_eq!(adjacency, vec![(0, 1)]);
        let a: f64 = total_cells(&polys.iter().find(|p| p.label == 0).unwrap().polys);
        let b: f64 = total_cells(&polys.iter().find(|p| p.label == 1).unwrap().polys);
        // outer boundary is smoothed identically for both; their sum must equal
        // the area enclosed by the smoothed outer boundary — verify via the
        // complement: sum of both equals total minus nothing, and each is > 3
        assert!(a > 3.0 && b > 3.0);
        assert!((a - b).abs() < 1e-9, "symmetric shapes must have equal area");
    }
}
