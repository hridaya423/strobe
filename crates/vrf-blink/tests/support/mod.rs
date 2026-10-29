use std::process::ExitCode;

use anyhow::Result;
use vrf_blink::Ui;

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

pub fn decode_audio(path: &std::path::Path) -> Result<Vec<f32>> {
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

pub fn has_audio_stream(path: &std::path::Path) -> Result<bool> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "ffprobe failed on {}",
        path.display()
    );
    Ok(!output.stdout.is_empty())
}

pub fn onset(stereo: &[f32], threshold: f32) -> Option<usize> {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .position(|frame| frame[0].abs() > threshold)
}

pub fn rms(stereo: &[f32], from: usize, frames: usize) -> f64 {
    let window = &stereo[from * 2..(from + frames) * 2];
    (window.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / window.len() as f64).sqrt()
}
