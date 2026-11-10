use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::driver::Plan;
use crate::{Ui, View};

const PROBE: &str = include_str!("../js/calibrate.html");
const SIZE: (i32, i32) = (1920, 1080);
const CANDIDATES: [usize; 4] = [1, 2, 4, 8];
const SAMPLE: Duration = Duration::from_millis(300);
const WARMUP: Duration = Duration::from_millis(50);
const GAIN: f64 = 1.25;
const CORES_PER_VIEW: usize = 4;
const FLOOR: f64 = 0.8;
const BASE: u64 = 1_000_003;

/// What a machine's measurement settles on: the views to run, and the frame rate measured at each
/// view count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub views: usize,
    pub fps: BTreeMap<usize, f64>,
}

#[cfg(not(windows))]
pub fn cache_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/vrf/calibration.json")
}

#[cfg(windows)]
pub fn cache_path() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("vrf/calibration.json")
}

#[cfg(not(windows))]
fn sysctl(name: &str) -> String {
    std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

#[cfg(not(windows))]
fn machine() -> String {
    format!(
        "{} {} cores {} bytes",
        sysctl("machdep.cpu.brand_string"),
        sysctl("hw.ncpu"),
        sysctl("hw.memsize")
    )
}

#[cfg(windows)]
fn machine() -> String {
    let var = |name| std::env::var(name).unwrap_or_default();
    format!(
        "{} {} cores",
        var("PROCESSOR_IDENTIFIER"),
        var("NUMBER_OF_PROCESSORS")
    )
}

/// Names the machine, the CEF build and the rasteriser, the three things a measurement is valid
/// for.
pub fn key() -> String {
    let cef = String::from_utf8_lossy(cef::sys::CEF_VERSION)
        .trim_end_matches('\0')
        .to_owned();
    format!(
        "{} cef {cef} {} raster{}",
        machine(),
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
/// over the last one kept. A machine with four cores per view or more takes that many views
/// anyway unless they cost it more than a fifth of the frame rate.
pub fn measure(ui: &Ui) -> Result<Calibration> {
    let page = std::env::temp_dir().join(format!("vrf-calibrate-{}.html", std::process::id()));
    fs::write(&page, PROBE)?;
    let measured = measure_page(ui, &page);
    let _ = fs::remove_file(&page);
    measured
}

fn measure_page(ui: &Ui, page: &Path) -> Result<Calibration> {
    let cores = thread::available_parallelism()?.get();
    let counts: Vec<usize> = CANDIDATES.into_iter().filter(|n| *n <= cores).collect();
    let url = format!(
        "file:///{}",
        page.display()
            .to_string()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
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
    Ok(Calibration {
        views: choose(&fps, cores),
        fps,
    })
}

fn choose(fps: &BTreeMap<usize, f64>, cores: usize) -> usize {
    let mut counts = fps.keys().copied();
    let mut chosen = counts.next().unwrap_or(1);
    for count in counts {
        if fps[&count] >= fps[&chosen] * GAIN {
            chosen = count;
        }
    }
    let roomy = fps
        .keys()
        .copied()
        .filter(|count| count * CORES_PER_VIEW <= cores)
        .last();
    match roomy {
        Some(count) if count > chosen && fps[&count] >= fps[&chosen] * FLOOR => count,
        _ => chosen,
    }
}

fn read_entries(cache: &Path) -> Result<BTreeMap<String, Calibration>> {
    match fs::read(cache) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid", cache.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", cache.display())),
    }
}

/// The cached measurement for this machine and CEF build, if there is one.
pub fn stored(cache: &Path) -> Option<Calibration> {
    read_entries(cache).ok()?.remove(&key())
}

fn record(cache: &Path, key: String, measured: Calibration) -> Result<()> {
    let mut entries = match read_entries(cache) {
        Ok(entries) => entries,
        Err(error) => {
            let kept = cache.with_extension("json.bad");
            fs::rename(cache, &kept)
                .with_context(|| format!("{error:#}, and it cannot be moved aside"))?;
            eprintln!("strobe: {error:#}; kept as {}", kept.display());
            BTreeMap::new()
        }
    };
    entries.insert(key, measured);
    fs::create_dir_all(cache.parent().context("cache path has no parent")?)?;
    let staging = cache.with_extension(format!("json.{}.tmp", std::process::id()));
    let written = fs::write(&staging, serde_json::to_vec_pretty(&entries)?)
        .and_then(|()| fs::rename(&staging, cache));
    if written.is_err() {
        let _ = fs::remove_file(&staging);
    }
    written.with_context(|| format!("write {}", cache.display()))
}

/// The cached measurement for this machine and CEF build, made and stored first if there is
/// none.
pub fn calibrated(ui: &Ui, cache: &Path) -> Result<Calibration> {
    if let Some(found) = stored(cache) {
        return Ok(found);
    }
    let measured = measure(ui)?;
    record(cache, key(), measured.clone())?;
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
    let views = match views {
        Some(views) => views,
        None => {
            let Some(calibration) = calibration()? else {
                return Ok(None);
            };
            calibration.views
        }
    };
    let run = run.unwrap_or_else(|| Plan::auto_run(lead_in));
    Ok(Some(Plan {
        lead_in,
        ..Plan::new(views, run)
    }))
}

/// The plan for a render: the flags' values where given, and the calibration's view count where
/// not. Nothing is measured when the view count is given.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn map(rates: [f64; 4]) -> BTreeMap<usize, f64> {
        CANDIDATES.into_iter().zip(rates).collect()
    }

    fn cache_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vrf-calibration-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(views: usize) -> Calibration {
        Calibration {
            views,
            fps: BTreeMap::from([(views, 1.0)]),
        }
    }

    #[test]
    fn recording_keeps_the_other_machines_entries() {
        let dir = cache_dir("keeps");
        let cache = dir.join("c.json");
        record(&cache, "other".into(), entry(2)).unwrap();
        record(&cache, "this".into(), entry(4)).unwrap();
        let entries = read_entries(&cache).unwrap();
        assert_eq!(entries["other"], entry(2));
        assert_eq!(entries["this"], entry(4));
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unreadable_cache_is_kept_aside_not_overwritten() {
        let dir = cache_dir("bad");
        let cache = dir.join("c.json");
        fs::write(&cache, "{ truncated").unwrap();
        assert!(read_entries(&cache).is_err());
        assert!(stored(&cache).is_none());
        record(&cache, "this".into(), entry(4)).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("c.json.bad")).unwrap(),
            "{ truncated"
        );
        assert_eq!(read_entries(&cache).unwrap()["this"], entry(4));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mac_mini_keeps_four_views() {
        let mini = map([170.8, 410.4, 653.4, 681.0]);
        assert_eq!(choose(&mini, 10), 4);
    }

    #[test]
    fn a_thirty_two_thread_windows_machine_takes_eight_views_on_either_rasteriser() {
        let gpu = map([69.3, 245.1, 422.9, 524.1]);
        let cpu = map([102.5, 269.9, 398.7, 379.6]);
        let cpu_again = map([113.7, 259.4, 383.1, 343.3]);
        assert_eq!(choose(&gpu, 32), 8);
        assert_eq!(choose(&cpu, 32), 8);
        assert_eq!(choose(&cpu_again, 32), 8);
    }

    #[test]
    fn fewer_cores_leave_the_gain_rule_alone() {
        let gpu = map([69.3, 245.1, 422.9, 524.1]);
        assert_eq!(choose(&gpu, 24), 4);
        assert_eq!(choose(&gpu, 16), 4);
    }

    #[test]
    fn views_that_cost_over_a_fifth_of_the_rate_are_not_taken_for_the_cores() {
        let slow = map([102.5, 269.9, 398.7, 300.0]);
        assert_eq!(choose(&slow, 32), 4);
    }

    #[test]
    fn a_clear_gain_wins_without_the_cores() {
        let gain = map([100.0, 190.0, 360.0, 800.0]);
        assert_eq!(choose(&gain, 8), 8);
    }

    #[test]
    fn one_view_stays_when_more_gain_nothing() {
        let flat = map([100.0, 101.0, 100.0, 99.0]);
        assert_eq!(choose(&flat, 4), 1);
    }
}
