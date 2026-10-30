use std::process::ExitCode;

use anyhow::Result;
use vrf::gpu::Gpu;
use vrf_blink::{Paint, Ui};

pub fn link_dir(
    target: impl AsRef<std::path::Path>,
    link: impl AsRef<std::path::Path>,
) -> Result<()> {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)?;
    #[cfg(windows)]
    {
        let target = std::fs::canonicalize(target)?;
        let target = target
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let link = std::path::absolute(link)?;
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "mklink /J failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub const DETERMINISTIC: (vrf_encode::Codec, &str) = (vrf_encode::Codec::ProRes422, "mov");
#[cfg(windows)]
pub const DETERMINISTIC: (vrf_encode::Codec, &str) = (vrf_encode::Codec::H264, "mp4");

pub type Case = (&'static str, fn(&Ui) -> Result<()>);

pub fn run(cases: &[Case]) -> ExitCode {
    let helper = std::env::args().any(|arg| arg.starts_with("--type="));
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !helper && !arg.starts_with('-'))
        .collect();
    let selected: Vec<Case> = cases
        .iter()
        .copied()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    if selected.is_empty() && !helper {
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
    let dir = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    format!(
        "file:///{}/tests/pages/colour.html",
        dir.trim_start_matches('/')
    )
}

#[cfg(target_os = "macos")]
pub fn import(gpu: &Gpu, paint: &Paint) -> Result<wgpu::Texture> {
    gpu.import_iosurface(&paint.surface, 0, paint.format)
}

#[cfg(windows)]
pub fn import(gpu: &Gpu, paint: &Paint) -> Result<wgpu::Texture> {
    gpu.import_shared_handle(&paint.surface, wgpu::TextureUsages::COPY_SRC)
}

pub fn pixel(paint: &Paint, x: usize, y: usize) -> Result<[u8; 3]> {
    static GPU: std::sync::OnceLock<Gpu> = std::sync::OnceLock::new();
    if GPU.get().is_none() {
        let _ = GPU.set(Gpu::new()?);
    }
    let gpu = GPU.get().expect("the GPU was just opened");
    let texture = import(gpu, paint)?;
    let width = texture.width() as usize;
    let bgra = gpu.read_texture(&texture)?;
    let at = (y * width + x) * 4;
    Ok([bgra[at + 2], bgra[at + 1], bgra[at]])
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

#[cfg(windows)]
pub fn load_average() -> String {
    "n/a".to_owned()
}

#[cfg(not(windows))]
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
