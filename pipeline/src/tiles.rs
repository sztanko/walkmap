use anyhow::{bail, Result};
use std::path::Path;
use std::process::Command;

/// One PMTiles archive per city per feature type; layers `partitions`
/// (all zooms) and `buildings` (z13+, set per-feature) in one file.
pub fn tippecanoe(out: &Path, partitions: &Path, buildings: &Path) -> Result<()> {
    let status = Command::new("tippecanoe")
        .arg("-o")
        .arg(out)
        .args([
            "--force",
            "--quiet",
            "--read-parallel",
            "-Z7",
            // z15 max keeps every archive under GitHub Pages' 100MB file cap;
            // MapLibre overzooms losslessly (~20 cm precision at z15)
            "-z15",
            "--detect-shared-borders",
            "--coalesce-smallest-as-needed",
            "--drop-densest-as-needed",
        ])
        .arg(partitions)
        .arg(buildings)
        .status()?;
    if !status.success() {
        bail!("tippecanoe failed with {status}");
    }
    Ok(())
}
