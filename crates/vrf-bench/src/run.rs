use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessesToUpdate, System};

use crate::corpus::{Corpus, bench_dir};
use crate::engines::Engine;

pub const MAX_LOAD: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Timing {
    pub median_s: f64,
    pub min_s: f64,
    pub max_s: f64,
    pub runs: usize,
}

#[derive(Deserialize)]
struct Hyperfine {
    results: Vec<HyperfineResult>,
}

#[derive(Deserialize)]
struct HyperfineResult {
    command: String,
    measurements: Vec<Measurement>,
    summary: Summary,
}

#[derive(Deserialize)]
struct Measurement {
    exit_code: i32,
}

#[derive(Deserialize)]
struct Summary {
    time_wall_clock: Stat,
}

#[derive(Deserialize)]
struct Stat {
    count: usize,
    median: f64,
    min: f64,
    max: f64,
}

impl Timing {
    pub fn from_hyperfine(json: &str) -> Result<Timing> {
        let h: Hyperfine = serde_json::from_str(json).context("parsing hyperfine JSON")?;
        let [r] = h.results.as_slice() else {
            bail!("expected one hyperfine result, got {}", h.results.len())
        };
        if let Some(m) = r.measurements.iter().find(|m| m.exit_code != 0) {
            bail!("`{}` exited with {}", r.command, m.exit_code);
        }
        let w = &r.summary.time_wall_clock;
        Ok(Timing {
            median_s: w.median,
            min_s: w.min,
            max_s: w.max,
            runs: w.count,
        })
    }

    pub fn fps(&self, frames: u32) -> f64 {
        frames as f64 / self.median_s
    }
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub engine: String,
    pub version: String,
    pub corpus_id: String,
    pub frames: u32,
    pub median_s: f64,
    pub min_s: f64,
    pub max_s: f64,
    pub fps: f64,
    pub machine: String,
    pub load_avg_before: f64,
}

pub fn machine() -> Result<String> {
    let mut sys = System::new()?;
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    let host = System::host_name().unwrap_or_default();
    Ok(format!(
        "{host} {cpu} ({} cores, {} GiB)",
        sys.cpus().len(),
        sys.total_memory() >> 30
    ))
}

#[cfg(windows)]
fn load_one() -> Result<f64> {
    let mut sys = System::new()?;
    sys.refresh_cpu_all();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_all();
    Ok(f64::from(sys.global_cpu_usage()) / 100.0 * sys.cpus().len() as f64)
}

#[cfg(not(windows))]
fn load_one() -> Result<f64> {
    Ok(System::load_average()?.one)
}

pub fn ensure_idle() -> Result<f64> {
    let load = load_one()?;
    if load < MAX_LOAD {
        return Ok(load);
    }
    let mut sys = System::new()?;
    sys.refresh_processes(ProcessesToUpdate::All, true);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut procs: Vec<_> = sys.processes().values().collect();
    procs.sort_by(|a, b| b.cpu_usage().total_cmp(&a.cpu_usage()));
    let top: Vec<String> = procs
        .iter()
        .take(3)
        .map(|p| format!("  {:6.1}%  {}", p.cpu_usage(), p.name().to_string_lossy()))
        .collect();
    bail!(
        "load average {load:.2} is at or over {MAX_LOAD}; busiest processes:\n{}",
        top.join("\n")
    )
}

#[cfg(windows)]
fn shell_quote(s: &str) -> String {
    format!("\"{s}\"")
}

#[cfg(not(windows))]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn remove_file_command(path: &Path) -> String {
    let path = shell_quote(&path.display().to_string());
    if cfg!(windows) {
        format!("if exist {path} del /f /q {path}")
    } else {
        format!("rm -f {path}")
    }
}

pub fn results_dir() -> PathBuf {
    bench_dir().join("results")
}

pub fn run(engine: Engine, corpus: &Corpus, runs: usize, out_dir: &Path) -> Result<RunResult> {
    ensure!(
        engine.supports(corpus.framework),
        "{} cannot render {} projects",
        engine.name(),
        corpus.id
    );
    let load = ensure_idle()?;
    std::fs::create_dir_all(out_dir)?;
    let out = out_dir.join(format!("{}-{}.mp4", engine.name(), corpus.id));
    let inv = engine.invocation(corpus, &out);
    let cmd = inv
        .argv
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let json = out_dir.join(format!("{}-{}.hyperfine.json", engine.name(), corpus.id));
    let status = Command::new("hyperfine")
        .current_dir(&inv.cwd)
        .env("HYPERFRAMES_NO_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .env("REMOTION_DISABLE_TELEMETRY", "1")
        .args([
            "--warmup",
            "1",
            "--runs",
            &runs.to_string(),
            "--style",
            "basic",
        ])
        .args(["--prepare", &remove_file_command(&out)])
        .args([
            "--command-name",
            &format!("{} {}", engine.name(), corpus.id),
        ])
        .arg("--export-json")
        .arg(&json)
        .arg(&cmd)
        .status()
        .context("running hyperfine (brew install hyperfine)")?;
    ensure!(status.success(), "hyperfine failed for `{cmd}`");
    let t = Timing::from_hyperfine(&std::fs::read_to_string(&json)?)?;
    Ok(RunResult {
        engine: engine.name().into(),
        version: engine.version().into(),
        corpus_id: corpus.id.clone(),
        frames: corpus.frames,
        fps: t.fps(corpus.frames),
        median_s: t.median_s,
        min_s: t.min_s,
        max_s: t.max_s,
        machine: machine()?,
        load_avg_before: load,
    })
}
