use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use vrf_bench::corpus::bench_dir;
use vrf_blink::adapters::remotion::Options;
use vrf_blink::driver::Plan;
use vrf_blink::{Mode, Ui};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/js/test/remotion/fixtures");

pub fn work_dir() -> PathBuf {
    std::env::temp_dir().join("vrf-remotion-tests")
}

pub fn fixture_project(name: &str) -> Result<PathBuf> {
    let project = work_dir().join(name);
    let modules = project.join("node_modules");
    if !modules.exists() {
        fs::create_dir_all(&project)?;
        fs::write(
            project.join("package.json"),
            format!(r#"{{"name":"vrf-{name}","private":true}}"#),
        )?;
        let reference = std::env::var_os("VRF_BENCH_DIR")
            .map_or_else(bench_dir, PathBuf::from)
            .join("corpus/external/github-unwrapped/node_modules");
        symlink(reference, &modules)?;
    }
    let src = project.join("src");
    fs::create_dir_all(&src)?;
    fs::copy(
        Path::new(FIXTURES).join(name).join("index.jsx"),
        src.join("index.jsx"),
    )?;
    Ok(project)
}

pub fn options(project: &Path, composition: &str) -> Options {
    Options {
        project: project.to_owned(),
        entry: Some("src/index.jsx".into()),
        composition: composition.into(),
        props: None,
        mode: Mode::Offline,
        timeout: Duration::from_millis(2500),
        plan: Plan::single(),
    }
}

pub type Case = (&'static str, fn(&Ui) -> Result<()>);

pub fn run(cases: &[Case]) -> ExitCode {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let selected: Vec<Case> = cases
        .iter()
        .copied()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    if selected.is_empty() {
        println!("running 0 tests\n\ntest result: ok. 0 passed; 0 failed");
        return ExitCode::SUCCESS;
    }
    let results = vrf_blink::run(move |ui| {
        println!("running {} tests", selected.len());
        selected
            .into_iter()
            .map(|(name, case)| {
                let result = case(&ui);
                match &result {
                    Ok(()) => println!("test {name} ... ok"),
                    Err(error) => println!("test {name} ... FAILED\n{error:#}"),
                }
                result.is_ok()
            })
            .collect::<Vec<_>>()
    });
    let passed = results.iter().filter(|ok| **ok).count();
    let failed = results.len() - passed;
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    println!("\ntest result: {verdict}. {passed} passed; {failed} failed");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const TOLERANCE: i32 = 3;

pub fn page_url() -> String {
    format!(
        "file://{}/tests/pages/colour.html",
        env!("CARGO_MANIFEST_DIR")
    )
}

pub fn expected_colour(index: u64) -> [i32; 3] {
    let digit = |d: u64| 16 + 10 * d as i32;
    [
        digit(index % 24),
        digit(index / 24 % 24),
        digit(index / 576),
    ]
}

pub fn centre_colours(path: &std::path::Path) -> Result<Vec<[i32; 3]>> {
    let crop = format!("crop=2:2:{}:{}", WIDTH / 2, HEIGHT / 2);
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", &format!("{crop},scale=in_color_matrix=bt709:in_range=limited:out_range=full:flags=accurate_rnd+full_chroma_int,format=rgb24")])
        .args(["-f", "rawvideo", "-"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output
        .stdout
        .as_chunks::<12>()
        .0
        .iter()
        .map(|frame| [frame[0] as i32, frame[1] as i32, frame[2] as i32])
        .collect())
}

pub fn load_average() -> String {
    let output = std::process::Command::new("uptime").output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.split("load averages:")
        .nth(1)
        .unwrap_or("?")
        .trim()
        .to_owned()
}
