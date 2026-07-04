use anyhow::{Context, Result};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Copernicus DEM GLO-30 (public AWS Open Data bucket), 1°×1° COG tiles named
/// by their SW corner. Whole tiles are downloaded and cached — a city touches
/// only a handful, and we need dense coverage anyway.
const BUCKET: &str = "https://copernicus-dem-30m.s3.amazonaws.com";

struct Tile {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

pub struct Dem {
    tiles: FxHashMap<(i32, i32), Option<Tile>>,
}

fn tile_name(lat0: i32, lng0: i32) -> String {
    let ns = if lat0 >= 0 { 'N' } else { 'S' };
    let ew = if lng0 >= 0 { 'E' } else { 'W' };
    format!(
        "Copernicus_DSM_COG_10_{}{:02}_00_{}{:03}_00_DEM",
        ns,
        lat0.abs(),
        ew,
        lng0.abs()
    )
}

/// Download via the system curl: it uses the platform trust store, which
/// matters behind TLS-intercepting proxies where rustls' bundled roots fail.
pub fn curl_download(url: &str, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("part");
    let status = std::process::Command::new("curl")
        .args(["-sSL", "--fail", "--retry", "3", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .context("running curl")?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("curl failed ({status}) for {url}");
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn fetch_tile(cache_dir: &Path, lat0: i32, lng0: i32) -> Result<Option<PathBuf>> {
    let name = tile_name(lat0, lng0);
    let path = cache_dir.join(format!("{name}.tif"));
    let missing = cache_dir.join(format!("{name}.missing"));
    if path.exists() {
        return Ok(Some(path));
    }
    if missing.exists() {
        return Ok(None);
    }
    let url = format!("{BUCKET}/{name}/{name}.tif");
    eprintln!("  dem: fetching {name}");
    match curl_download(&url, &path) {
        Ok(()) => Ok(Some(path)),
        Err(e) if e.to_string().contains("exit status: 22") => {
            // curl --fail exits 22 on HTTP 4xx: open ocean — no tile exists
            std::fs::write(&missing, b"")?;
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn decode_tile(path: &Path) -> Result<Tile> {
    let file = std::fs::File::open(path)?;
    let mut dec = tiff::decoder::Decoder::new(BufReader::new(file))?;
    let (w, h) = dec.dimensions()?;
    let img = dec.read_image()?;
    let data = match img {
        tiff::decoder::DecodingResult::F32(v) => v,
        tiff::decoder::DecodingResult::F64(v) => v.into_iter().map(|x| x as f32).collect(),
        tiff::decoder::DecodingResult::I16(v) => v.into_iter().map(|x| x as f32).collect(),
        other => anyhow::bail!("unexpected DEM sample format: {:?} in {}", kind(&other), path.display()),
    };
    Ok(Tile { w: w as usize, h: h as usize, data })
}

fn kind(d: &tiff::decoder::DecodingResult) -> &'static str {
    use tiff::decoder::DecodingResult::*;
    match d {
        U8(_) => "u8",
        U16(_) => "u16",
        U32(_) => "u32",
        U64(_) => "u64",
        I8(_) => "i8",
        I16(_) => "i16",
        I32(_) => "i32",
        I64(_) => "i64",
        F32(_) => "f32",
        F64(_) => "f64",
    }
}

impl Dem {
    /// Load (download + decode) every 1° tile touched by the given points.
    pub fn load_for(cache_dir: &Path, pts: &[[f64; 2]]) -> Result<Dem> {
        std::fs::create_dir_all(cache_dir)?;
        let mut want: FxHashSet<(i32, i32)> = FxHashSet::default();
        for p in pts {
            want.insert((p[1].floor() as i32, p[0].floor() as i32));
        }
        let mut tiles = FxHashMap::default();
        for (lat0, lng0) in want {
            let tile = match fetch_tile(cache_dir, lat0, lng0)? {
                Some(path) => Some(decode_tile(&path).with_context(|| format!("decoding {}", path.display()))?),
                None => None,
            };
            tiles.insert((lat0, lng0), tile);
        }
        Ok(Dem { tiles })
    }

    /// Bilinear elevation sample; 0.0 over missing (ocean) tiles.
    pub fn sample(&self, lng: f64, lat: f64) -> f32 {
        let key = (lat.floor() as i32, lng.floor() as i32);
        let Some(Some(tile)) = self.tiles.get(&key) else {
            return 0.0;
        };
        // pixel centers at (lng0 + (i+0.5)/w, lat0+1 − (j+0.5)/h)
        let fx = (lng - key.1 as f64) * tile.w as f64 - 0.5;
        let fy = (key.0 as f64 + 1.0 - lat) * tile.h as f64 - 0.5;
        let x0 = fx.floor().clamp(0.0, (tile.w - 1) as f64) as usize;
        let y0 = fy.floor().clamp(0.0, (tile.h - 1) as f64) as usize;
        let x1 = (x0 + 1).min(tile.w - 1);
        let y1 = (y0 + 1).min(tile.h - 1);
        let tx = (fx - x0 as f64).clamp(0.0, 1.0) as f32;
        let ty = (fy - y0 as f64).clamp(0.0, 1.0) as f32;
        let at = |x: usize, y: usize| -> f32 {
            let v = tile.data[y * tile.w + x];
            if v < -1000.0 {
                0.0 // void
            } else {
                v
            }
        };
        let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
        let bot = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
        top * (1.0 - ty) + bot * ty
    }

    pub fn sample_all(&self, pts: &[[f64; 2]]) -> Vec<f32> {
        pts.par_iter().map(|p| self.sample(p[0], p[1])).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_names() {
        assert_eq!(tile_name(51, -1), "Copernicus_DSM_COG_10_N51_00_W001_00_DEM");
        assert_eq!(tile_name(32, -17), "Copernicus_DSM_COG_10_N32_00_W017_00_DEM");
        assert_eq!(tile_name(-34, 18), "Copernicus_DSM_COG_10_S34_00_E018_00_DEM");
    }
}
