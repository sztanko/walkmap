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
        nearest
            .par_chunks_mut(w as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let lat = north - (y as f64 + 0.5) * dlat;
                for (x, cell) in row.iter_mut().enumerate() {
                    let lng = west + (x as f64 + 0.5) * dlng;
                    if let Some((idx, _)) = snapper.nearest([lng, lat], max_m) {
                        *cell = idx;
                    }
                }
            });
        Grid { w, h, west, north, dlng, dlat, nearest }
    }

    /// grid-corner coordinates -> lng/lat
    pub fn corner_ll(&self, x: f64, y: f64) -> [f64; 2] {
        [self.west + x * self.dlng, self.north - y * self.dlat]
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
