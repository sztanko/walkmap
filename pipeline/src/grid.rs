use crate::snap::{Snapper, KY};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const NODATA: u32 = u32::MAX;

/// Once-per-city raster: every cell holds the index of the nearest graph node
/// (or NODATA if none within `max_m`). Per-feature-type partition labels are
/// then a pure array gather over this.
#[derive(Serialize, Deserialize)]
pub struct Grid {
    pub w: u32,
    pub h: u32,
    pub west: f64,
    pub north: f64,
    pub dlng: f64,
    pub dlat: f64,
    pub nearest: Vec<u32>,
    /// snap distance to the nearest node, decimeters (u16::MAX where NODATA)
    pub dist_dm: Vec<u16>,
}

impl Grid {
    pub fn build(snapper: &Snapper, bbox: [f64; 4], grid_m: f64, max_m: f64) -> Grid {
        let [west, south, east, north] = bbox;
        let latc = (south + north) / 2.0;
        let dlat = grid_m / KY;
        let dlng = grid_m / (111_320.0 * latc.to_radians().cos());
        let w = (((east - west) / dlng).ceil() as u32).max(1);
        let h = (((north - south) / dlat).ceil() as u32).max(1);
        let mut nearest = vec![NODATA; (w as usize) * (h as usize)];
        let mut dist_dm = vec![u16::MAX; (w as usize) * (h as usize)];
        nearest
            .par_chunks_mut(w as usize)
            .zip(dist_dm.par_chunks_mut(w as usize))
            .enumerate()
            .for_each(|(y, (row, drow))| {
                let lat = north - (y as f64 + 0.5) * dlat;
                for (x, (cell, dcell)) in row.iter_mut().zip(drow.iter_mut()).enumerate() {
                    let lng = west + (x as f64 + 0.5) * dlng;
                    if let Some((idx, d)) = snapper.nearest([lng, lat], max_m) {
                        *cell = idx;
                        *dcell = (d * 10.0).round().min(65534.0) as u16;
                    }
                }
            });
        Grid { w, h, west, north, dlng, dlat, nearest, dist_dm }
    }

    /// grid-corner coordinates -> lng/lat
    pub fn corner_ll(&self, x: f64, y: f64) -> [f64; 2] {
        [self.west + x * self.dlng, self.north - y * self.dlat]
    }

    fn cell_of(&self, ll: [f64; 2]) -> (i64, i64) {
        let x = ((ll[0] - self.west) / self.dlng).floor() as i64;
        let y = ((self.north - ll[1]) / self.dlat).floor() as i64;
        (x.clamp(0, self.w as i64 - 1), y.clamp(0, self.h as i64 - 1))
    }

    /// Per-cell walk direction toward the defining feature: 0 = terminal or
    /// no data, 1–8 = one step (N,NE,E,SE,S,SW,W,NW).
    ///
    /// Construction: the Dijkstra shortest-path tree (`next_hop` per node) is
    /// PAINTED into the raster — a Bresenham line of direction steps from
    /// every node's cell to its next hop's cell, walked in decreasing-time
    /// order so cells shared by several streets keep the fastest one. Cells
    /// not on any street then point one step toward their nearest node's
    /// cell. (Time-descent per cell dead-ends at road nodes, and
    /// head-to-your-node rules bounce straight back — the painted tree is
    /// the only encoding whose traces actually arrive at the site.)
    pub fn direction_field(&self, node_ll: &[[f64; 2]], next_hop: &[u32], node_dist_ds: &[u32]) -> Vec<u8> {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut dirs = vec![0u8; w * h];
        let code = |dx: i64, dy: i64| -> u8 {
            match (dx, dy) {
                (0, -1) => 1,
                (1, -1) => 2,
                (1, 0) => 3,
                (1, 1) => 4,
                (0, 1) => 5,
                (-1, 1) => 6,
                (-1, 0) => 7,
                (-1, -1) => 8,
                _ => 0,
            }
        };

        // Paint tree edges with per-cell TIME priority: a cell keeps the
        // direction of whichever edge passes through it at the earliest
        // interpolated walking time. Any branch switch during a trace then
        // strictly decreases remaining time — loops are impossible (pure
        // edge-priority painting let traces hop onto slower branches at
        // street crossings and orbit blocks).
        let mut best = vec![f32::INFINITY; w * h];
        for n in 0..node_ll.len() {
            if node_dist_ds[n] == u32::MAX || next_hop[n] == u32::MAX {
                continue;
            }
            let (x0, y0) = self.cell_of(node_ll[n]);
            let (tx, ty) = self.cell_of(node_ll[next_hop[n] as usize]);
            let t0 = node_dist_ds[n] as f32;
            let t1 = node_dist_ds[next_hop[n] as usize] as f32;
            let steps_total = (tx - x0).abs().max((ty - y0).abs()).max(1) as f32;
            let (mut x, mut y) = (x0, y0);
            let (dx, dy) = ((tx - x).abs(), -(ty - y).abs());
            let (sx, sy) = ((tx - x).signum(), (ty - y).signum());
            let mut err = dx + dy;
            let mut step_i = 0f32;
            // 8-connected Bresenham from (x,y) to (tx,ty)
            while (x, y) != (tx, ty) {
                let e2 = 2 * err;
                let (mut stepx, mut stepy) = (0i64, 0i64);
                if e2 >= dy {
                    err += dy;
                    stepx = sx;
                }
                if e2 <= dx {
                    err += dx;
                    stepy = sy;
                }
                if stepx == 0 && stepy == 0 {
                    break; // degenerate (same cell)
                }
                let cell = y as usize * w + x as usize;
                let t_here = t0 + (t1 - t0) * (step_i / steps_total);
                if t_here < best[cell] {
                    best[cell] = t_here;
                    dirs[cell] = code(stepx, stepy);
                }
                x += stepx;
                y += stepy;
                step_i += 1.0;
            }
        }
        drop(best);

        // site nodes are terminal — force their cells clear
        for n in 0..node_ll.len() {
            if node_dist_ds[n] == 0 {
                let (x, y) = self.cell_of(node_ll[n]);
                dirs[y as usize * w + x as usize] = 0;
            }
        }

        // off-street cells: one step toward the nearest node's cell
        dirs.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, d) in row.iter_mut().enumerate() {
                if *d != 0 {
                    continue;
                }
                let n = self.nearest[y * w + x];
                if n == NODATA || node_dist_ds[n as usize] == u32::MAX || node_dist_ds[n as usize] == 0 {
                    continue;
                }
                let (tx, ty) = self.cell_of(node_ll[n as usize]);
                if (tx, ty) == (x as i64, y as i64) {
                    continue;
                }
                *d = code((tx - x as i64).signum(), (ty - y as i64).signum());
            }
        });
        dirs
    }

    pub fn bbox(&self) -> [f64; 4] {
        [
            self.west,
            self.north - self.h as f64 * self.dlat,
            self.west + self.w as f64 * self.dlng,
            self.north,
        ]
    }
}
