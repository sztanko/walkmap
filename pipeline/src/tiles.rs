use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::path::PathBuf;
use std::process::Command;

/// Concurrent tippecanoe jobs. Each job is one feature type's archive.
pub const PARALLEL_JOBS: usize = 8;

/// FlatGeobuf can't carry per-feature `tippecanoe` objects (the GeoJSON trick
/// for per-layer zoom ranges), so partitions (z7+) and buildings (z12+) are
/// tiled in separate runs and merged with tile-join.
pub struct TileJob {
    pub label: String,
    pub out: PathBuf,
    pub partitions_fgb: PathBuf,
    pub buildings_fgb: PathBuf,
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().context("spawning tippecanoe/tile-join")?;
    if !out.status.success() {
        bail!(
            "{:?} failed ({}): {}",
            cmd.get_program(),
            out.status,
            String::from_utf8_lossy(&out.stderr).chars().take(600).collect::<String>()
        );
    }
    Ok(())
}

fn run_one(j: &TileJob) -> Result<()> {
    let parts_tmp = j.out.with_extension("parts.pmtiles");
    let blds_tmp = j.out.with_extension("blds.pmtiles");
    run(Command::new("tippecanoe")
        .arg("-o")
        .arg(&parts_tmp)
        .args([
            "--force",
            "--quiet",
            "-l",
            "partitions",
            "-Z7",
            "-z15",
            "--detect-shared-borders",
            "--coalesce-smallest-as-needed",
            "--tiny-polygon-size=4",
        ])
        .arg(&j.partitions_fgb))?;
    run(Command::new("tippecanoe")
        .arg("-o")
        .arg(&blds_tmp)
        .args([
            "--force",
            "--quiet",
            "-l",
            "buildings",
            "-Z12",
            "-z15",
            "--drop-densest-as-needed",
            "--tiny-polygon-size=4",
        ])
        .arg(&j.buildings_fgb))?;
    run(Command::new("tile-join")
        .arg("-o")
        .arg(&j.out)
        .args(["--force", "--quiet", "-pk"]) // components already size-limited
        .arg(&parts_tmp)
        .arg(&blds_tmp))?;
    for f in [&parts_tmp, &blds_tmp, &j.partitions_fgb, &j.buildings_fgb] {
        let _ = std::fs::remove_file(f);
    }
    eprintln!(
        "  tiles {}: {} MB",
        j.label,
        std::fs::metadata(&j.out).map(|m| m.len() / 1_048_576).unwrap_or(0)
    );
    Ok(())
}

/// Run all jobs, PARALLEL_JOBS at a time. Fails if any job failed.
pub fn run_jobs(jobs: Vec<TileJob>) -> Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(PARALLEL_JOBS.min(jobs.len()))
        .build()?;
    let results: Vec<(String, Result<()>)> =
        pool.install(|| jobs.par_iter().map(|j| (j.label.clone(), run_one(j))).collect());
    let mut failed = 0;
    for (label, r) in results {
        if let Err(e) = r {
            eprintln!("  TILES FAILED {label}: {e:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} tile job(s) failed");
    }
    Ok(())
}
