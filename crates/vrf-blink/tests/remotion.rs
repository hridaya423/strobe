#[allow(dead_code)]
mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use vrf::gpu::Gpu;
use vrf_bench::corpus::bench_dir;
use vrf_bench::engines::{headless_shell, node_bin};
use vrf_bench::score::{self, Sampling};
use vrf_blink::adapters::remotion::{self, Options, Page};
use vrf_blink::driver::Plan;
use vrf_blink::{Mode, Project, Ui};
use vrf_encode::Codec;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/js/test/remotion/fixtures");

fn work_dir() -> PathBuf {
    std::env::temp_dir().join("vrf-remotion-tests")
}

fn fixture_project(name: &str) -> Result<PathBuf> {
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

fn options(project: &Path, composition: &str) -> Options {
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

fn still(project: &Path, composition: &str, frame: u64, out: &Path) -> Result<()> {
    let output = Command::new(node_bin(project, "remotion"))
        .current_dir(project)
        .env("REMOTION_DISABLE_TELEMETRY", "1")
        .args(["still", "src/index.jsx", composition])
        .arg(out)
        .arg(format!("--frame={frame}"))
        .args(["--image-format", "png", "--browser-executable"])
        .arg(headless_shell())
        .output()?;
    ensure!(
        output.status.success(),
        "remotion still failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn frames_of(path: &Path) -> Result<Vec<score::Rgb8>> {
    score::open(path)?
        .frames
        .map(|frame| Ok(frame?.1))
        .collect()
}

fn pixel(frame: &score::Rgb8, x: u32, y: u32) -> [i32; 3] {
    let at = ((y * frame.width + x) * 3) as usize;
    [
        frame.data[at] as i32,
        frame.data[at + 1] as i32,
        frame.data[at + 2] as i32,
    ]
}

fn near(got: [i32; 3], want: [i32; 3], tolerance: i32) -> Result<()> {
    ensure!(
        got.iter()
            .zip(want)
            .all(|(a, b)| (a - b).abs() <= tolerance),
        "pixel {got:?}, expected {want:?} within {tolerance}"
    );
    Ok(())
}

fn stills_of(project: &Path, composition: &str, frames: &[u64], name: &str) -> Result<PathBuf> {
    let dir = work_dir().join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    for &frame in frames {
        still(
            project,
            composition,
            frame,
            &dir.join(format!("{frame:06}.png")),
        )?;
    }
    Ok(dir)
}

fn reference_mode_pixels_equal_remotion_still(ui: &Ui) -> Result<()> {
    let project = fixture_project("gray")?;
    let frames = [0, 1, 29, 44, 59];
    let stills = stills_of(&project, "Gray", &frames, "gray-pixel-stills")?;
    let bundle = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    let page = Page::new(
        Project::open(&bundle.dir, Mode::Offline)?,
        &options(&project, "Gray"),
        None,
    )?;
    let (mut view, _) = page.open(ui, 320, 180)?;
    let gpu = Gpu::new()?;
    for frame in frames {
        Page::seek(&mut view, frame, 1)?;
        view.begin_frame();
        let paint = view
            .recv_paint(Duration::from_secs(5))
            .context("no delivery")?;
        let texture = support::import(&gpu, &paint)?;
        let ours = score::Rgb8 {
            width: 320,
            height: 180,
            data: gpu
                .read_texture(&texture)?
                .chunks_exact(4)
                .flat_map(|bgra| [bgra[2], bgra[1], bgra[0]])
                .collect(),
        };
        let theirs = score::Rgb8::load_png(&stills.join(format!("{frame:06}.png")))?;
        let delta = score::compare(&ours, &theirs)?.max_delta;
        ensure!(
            delta == 0,
            "frame {frame} differs from remotion still by up to {delta}"
        );
    }
    view.close()
}

fn h264_render_stays_near_the_codec_floor(ui: &Ui) -> Result<()> {
    let project = fixture_project("gray")?;
    let out = work_dir().join("gray.mp4");
    let report = remotion::render(ui, &options(&project, "Gray"), None, Codec::H264, &out)?;
    println!("{}", serde_json::to_string(&report)?);
    ensure!(report.frames == 60 && report.repeats == 0 && report.seek_retries == 0);
    ensure!(
        frames_of(&out)?.len() == 60,
        "the file does not hold 60 frames"
    );

    let stills = stills_of(&project, "Gray", &[0, 1, 29, 44, 59], "gray-stills")?;
    let scored = score::score(&out, &stills, Sampling::All, None)?;
    let worst = &scored.worst;
    println!(
        "gray h264 vs remotion still: FLIP mean {:.5}, p99.9 {:.4}, max delta {}, over 8 {:.4}%, over 32 {:.4}%",
        worst.flip_mean.value,
        worst.flip_p999.value,
        worst.max_delta.value,
        worst.frac_over_8.value * 100.0,
        worst.frac_over_32.value * 100.0
    );
    ensure!(
        scored.frames_compared == 5,
        "{} frames compared",
        scored.frames_compared
    );
    ensure!(
        worst.flip_mean.value <= 0.025,
        "FLIP mean {}",
        worst.flip_mean.value
    );
    ensure!(
        worst.frac_over_32.value <= 0.0005,
        "{} of pixels over 32",
        worst.frac_over_32.value
    );
    ensure!(worst.ssim.value >= 0.98, "SSIM {}", worst.ssim.value);
    Ok(())
}

fn frame_range_renders_the_slice(ui: &Ui) -> Result<()> {
    let project = fixture_project("basic")?;
    let out = work_dir().join("basic-range.mp4");
    let report = remotion::render(
        ui,
        &options(&project, "Basic"),
        Some(30..=39),
        Codec::H264,
        &out,
    )?;
    ensure!(report.frames == 10 && report.first_frame == 30);
    let frames = frames_of(&out)?;
    ensure!(frames.len() == 10, "{} frames in the file", frames.len());
    let colour = |frame: i32| [frame * 2, 40, 255 - frame * 2];
    near(pixel(&frames[0], 300, 170), colour(30), 4)?;
    near(pixel(&frames[9], 300, 170), colour(39), 4)
}

fn failed_frame_is_retried_on_a_fresh_page(ui: &Ui) -> Result<()> {
    let project = fixture_project("retry")?;
    let out = work_dir().join("retry.mp4");
    let report = remotion::render(ui, &options(&project, "Flaky"), None, Codec::H264, &out)?;
    ensure!(
        report.frames == 10 && report.seek_retries == 1,
        "{} retries",
        report.seek_retries
    );
    let frames = frames_of(&out)?;
    ensure!(frames.len() == 10);
    for (index, frame) in frames.iter().enumerate() {
        let i = index as i32;
        near(pixel(frame, 5, 5), [i * 10, 40, 255 - i * 10], 4)
            .with_context(|| format!("frame {index}"))?;
    }
    Ok(())
}

fn assets_are_collected_and_resolved(ui: &Ui) -> Result<()> {
    let project = fixture_project("assets")?;
    let out = work_dir().join("assets.mp4");
    let report = remotion::render(
        ui,
        &options(&project, "Offset"),
        Some(0..=35),
        Codec::H264,
        &out,
    )?;
    let at = |frame: usize| &report.assets[frame].assets;
    ensure!(
        at(5).is_empty(),
        "assets before any sequence starts: {:?}",
        at(5)
    );
    let long = format!("http://assets.invalid/{}.mp3", "a".repeat(500));
    for frame in 10..30 {
        let assets = at(frame);
        ensure!(
            assets.len() == 1 && assets[0]["src"] == long.as_str(),
            "frame {frame}: {assets:?}"
        );
    }
    let offset = at(31);
    ensure!(
        offset.len() == 1
            && offset[0]["src"] == "http://assets.invalid/offset.mp3"
            && offset[0]["playbackRate"] == 2,
        "frame 31: {offset:?}"
    );
    Ok(())
}

fn parallel_views_write_the_same_bytes_as_one_view(ui: &Ui) -> Result<()> {
    let project = fixture_project("basic")?;
    let mut files = Vec::new();
    for views in [1, 2, 4, 8] {
        let out = work_dir().join(format!("basic-views-{views}.mov"));
        let mut options = options(&project, "Basic");
        options.plan = Plan::new(views, 11);
        let report = remotion::render(ui, &options, None, Codec::ProRes422, &out)?;
        ensure!(report.frames == 90 && report.views == views && report.repeats == 0);
        files.push((views, fs::read(&out)?));
    }
    for (views, bytes) in &files[1..] {
        ensure!(
            *bytes == files[0].1,
            "{views} views wrote different bytes from one view"
        );
    }
    Ok(())
}

fn bundle_is_cached_by_input(_: &Ui) -> Result<()> {
    let project = fixture_project("basic")?;
    let first = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    let second = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    ensure!(
        second.cached && second.dir == first.dir,
        "an unchanged project was bundled again"
    );
    let source = project.join("src/index.jsx");
    let text = fs::read_to_string(&source)?;
    fs::write(&source, format!("{text}\n"))?;
    let third = remotion::bundle(&project, Some(Path::new("src/index.jsx")))?;
    ensure!(
        !third.cached && third.dir != first.dir,
        "a changed source reused the old bundle"
    );
    fs::write(&source, text)?;
    Ok(())
}

fn unknown_composition_fails_with_its_name(ui: &Ui) -> Result<()> {
    let project = fixture_project("basic")?;
    let out = work_dir().join("unknown.mp4");
    let Err(error) = remotion::render(ui, &options(&project, "Missing"), None, Codec::H264, &out)
    else {
        anyhow::bail!("an unknown composition rendered");
    };
    let text = format!("{error:#}");
    ensure!(
        text.contains("Missing"),
        "the error does not name the composition: {text}"
    );
    Ok(())
}

fn main() -> ExitCode {
    vrf_blink::set_cpu_raster(true);
    support::run(&[
        (
            "remotion_reference_mode_pixels_equal_still",
            reference_mode_pixels_equal_remotion_still,
        ),
        (
            "remotion_h264_stays_near_codec_floor",
            h264_render_stays_near_the_codec_floor,
        ),
        ("remotion_frame_range", frame_range_renders_the_slice),
        (
            "remotion_retry_on_fresh_page",
            failed_frame_is_retried_on_a_fresh_page,
        ),
        ("remotion_assets", assets_are_collected_and_resolved),
        (
            "remotion_parallel_views_match_one_view",
            parallel_views_write_the_same_bytes_as_one_view,
        ),
        ("remotion_bundle_cache", bundle_is_cached_by_input),
        (
            "remotion_unknown_composition",
            unknown_composition_fails_with_its_name,
        ),
    ])
}
