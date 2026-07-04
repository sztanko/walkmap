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

    /// Per-cell walk direction toward the defining feature: 0 = terminal or
    /// no data, 1–8 = the 8-neighbour (N,NE,E,SE,S,SW,W,NW) with the strictly
    /// smallest walking time. `node_dist_ds` is the Dijkstra result
    /// (deciseconds, u32::MAX = unreached). The field descends monotonically,
    /// terminating at the feature's cells.
    pub fn direction_field(&self, node_dist_ds: &[u32]) -> Vec<u8> {
        let (w, h) = (self.w as usize, self.h as usize);
        let t: Vec<f32> = self
            .nearest
            .par_iter()
            .zip(self.dist_dm.par_iter())
            .map(|(&n, &dm)| {
                if n == NODATA || node_dist_ds[n as usize] == u32::MAX {
                    f32::INFINITY
                } else {
                    node_dist_ds[n as usize] as f32 / 10.0 + dm as f32 / 10.0 / 1.39
                }
            })
            .collect();
        const NB: [(isize, isize); 8] =
            [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)];
        let mut dirs = vec![0u8; w * h];
        dirs.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, d) in row.iter_mut().enumerate() {
                let own = t[y * w + x];
                if !own.is_finite() {
                    continue;
                }
                let mut best = (own, 0u8);
                for (i, (dx, dy)) in NB.iter().enumerate() {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let nt = t[ny as usize * w + nx as usize];
                    if nt < best.0 {
                        best = (nt, (i + 1) as u8);
                    }
                }
                *d = best.1;
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
