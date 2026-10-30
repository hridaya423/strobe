use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::driver::{LEAD_IN, Plan};
use crate::{Ui, View};

const PROBE: &str = include_str!("../js/calibrate.html");
const SIZE: (i32, i32) = (1920, 1080);
const CANDIDATES: [usize; 4] = [1, 2, 4, 8];
const SAMPLE: Duration = Duration::from_millis(300);
const WARMUP: Duration = Duration::from_millis(50);
const GAIN: f64 = 1.25;
const BASE: u64 = 1_000_003;
const LEAD_IN_SHARE: f64 = 0.25;

/// What a machine's measurement settles on: the views to run, the frames each renders in a row,
/// and the frame rate measured at each view count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub views: usize,
    pub run: u64,
    pub fps: BTreeMap<usize, f64>,
}

pub fn cache_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/vrf/calibration.json")
}

fn sysctl(name: &str) -> String {
    Command::new("sysctl")
        .args(["-n", name])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// Names the machine, the CEF build and the rasteriser, the three things a measurement is valid
/// for.
pub fn key() -> String {
    let cef = String::from_utf8_lossy(cef::sys::CEF_VERSION)
        .trim_end_matches('\0')
        .to_owned();
    format!(
        "{} {} cores {} bytes cef {cef} {} raster{}",
        sysctl("machdep.cpu.brand_string"),
        sysctl("hw.ncpu"),
        sysctl("hw.memsize"),
        if crate::cpu_raster() { "cpu" } else { "gpu" },
        if crate::software_webgl() {
            " swiftshader"
        } else {
            ""
        }
    )
}

fn frames_in(view: &mut View, deadline: Instant, base: u64) -> Result<u64> {
    let mut frames = 0;
    while Instant::now() < deadline {
        view.evaluate(&format!("seek({})", base + frames))?;
        view.begin_frame();
        view.recv_paint(Duration::from_secs(5))
            .context("the probe page delivered no frame")?;
        frames += 1;
    }
    Ok(frames)
}

/// Runs a page of a hundred and fifty moving boxes in up to eight views, one view count at a time, for
/// about two seconds in all, and keeps a view count only if it raises the frame rate by 25%
/// over the last one kept.
pub fn measure(ui: &Ui) -> Result<Calibration> {
    let cores = thread::available_parallelism()?.get();
    let counts: Vec<usize> = CANDIDATES.into_iter().filter(|n| *n <= cores).collect();
    let page = std::env::temp_dir().join("vrf-calibrate.html");
    fs::write(&page, PROBE)?;
    let url = format!("file://{}", page.display());
    let most = *counts.last().context("no view count to try")?;
    let mut views = thread::scope(|scope| {
        let opens: Vec<_> = (0..most)
            .map(|_| scope.spawn(|| View::open(ui, &url, SIZE.0, SIZE.1)))
            .collect();
        opens
            .into_iter()
            .map(|open| open.join().expect("view thread panicked"))
            .collect::<Result<Vec<_>>>()
    })?;
    let mut fps = BTreeMap::new();
    for &count in &counts {
        let deadline = Instant::now() + WARMUP;
        thread::scope(|scope| -> Result<()> {
            let warm: Vec<_> = views[..count]
                .iter_mut()
                .map(|view| scope.spawn(move || frames_in(view, deadline, 0)))
                .collect();
            warm.into_iter()
                .try_for_each(|w| w.join().expect("view thread panicked").map(drop))
        })?;
        let started = Instant::now();
        let deadline = started + SAMPLE;
        let frames: u64 = thread::scope(|scope| {
            let runs: Vec<_> = views[..count]
                .iter_mut()
                .map(|view| scope.spawn(move || frames_in(view, deadline, BASE)))
                .collect();
            runs.into_iter()
                .map(|run| run.join().expect("view thread panicked"))
                .sum::<Result<u64>>()
        })?;
        let rate = frames as f64 / started.elapsed().as_secs_f64();
        fps.insert(count, (rate * 10.0).round() / 10.0);
    }
    for view in views {
        view.close()?;
    }
    let mut chosen = 1;
    for &count in counts.iter().skip(1) {
        if fps[&count] >= fps[&chosen] * GAIN {
            chosen = count;
        }
    }
    Ok(Calibration {
        views: chosen,
        run: (LEAD_IN as f64 / LEAD_IN_SHARE).ceil() as u64,
        fps,
    })
}

fn read_entries(cache: &Path) -> BTreeMap<String, Calibration> {
    fs::read(cache)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The cached measurement for this machine and CEF build, if there is one.
pub fn stored(cache: &Path) -> Option<Calibration> {
    read_entries(cache).remove(&key())
}

/// The cached measurement for this machine and CEF build, made and stored first if there is
/// none.
pub fn calibrated(ui: &Ui, cache: &Path) -> Result<Calibration> {
    if let Some(found) = stored(cache) {
        return Ok(found);
    }
    let measured = measure(ui)?;
    let mut entries = read_entries(cache);
    entries.insert(key(), measured.clone());
    fs::create_dir_all(cache.parent().context("cache path has no parent")?)?;
    fs::write(cache, serde_json::to_vec_pretty(&entries)?)?;
    Ok(measured)
}

fn plan_from(
    views: Option<usize>,
    run: Option<u64>,
    lead_in: u64,
    calibration: impl FnOnce() -> Result<Option<Calibration>>,
) -> Result<Option<Plan>> {
    ensure!(
        views != Some(0) && run != Some(0),
        "views and run must be at least 1"
    );
    let (views, run) = match (views, run) {
        (Some(views), Some(run)) => (views, run),
        _ => {
            let Some(calibration) = calibration()? else {
                return Ok(None);
            };
            (
                views.unwrap_or(calibration.views),
                run.unwrap_or(calibration.run),
            )
        }
    };
    Ok(Some(Plan {
        lead_in,
        ..Plan::new(views, run)
    }))
}

/// The plan for a render: the flags' values where given, and the calibration's where not. Nothing
/// is measured when both are given.
pub fn plan(ui: &Ui, views: Option<usize>, run: Option<u64>, lead_in: u64) -> Result<Plan> {
    plan_from(views, run, lead_in, || {
        calibrated(ui, &cache_path()).map(Some)
    })?
    .context("no calibration")
}

/// Like [`plan`], but `None` where it would have to measure, which needs a running CEF.
pub fn cached_plan(views: Option<usize>, run: Option<u64>, lead_in: u64) -> Result<Option<Plan>> {
    plan_from(views, run, lead_in, || Ok(stored(&cache_path())))
}
