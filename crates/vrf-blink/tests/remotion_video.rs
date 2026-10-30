#[allow(dead_code)]
mod support;
mod clips;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clips::clip;
use vrf::gpu::Gpu;
use vrf_bench::corpus::bench_dir;
use vrf_bench::score;
use vrf_blink::adapters::remotion::{self, Fallback, Offthread, Options, Page, VideoDecoder};
use vrf_blink::driver::Plan;
use vrf_blink::{Mode, Project, Ui};
use vrf_encode::Codec;

const CLIPS: [&str; 6] = [
    "cfr1080.mp4",
    "vfr1080.mp4",
    "off1080.mp4",
    "cfr1080-hevc.mp4",
    "cfr1080-601.mp4",
    "cfr1080-full.mp4",
];
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/js/test/remotion/fixtures/video/index.jsx");

fn project() -> Result<PathBuf> {
    let project = std::env::temp_dir().join("vrf-remotion-video-tests/video");
    if !project.join("node_modules").exists() {
        fs::create_dir_all(&project)?;
        fs::write(project.join("package.json"), r#"{"name":"vrf-video","private":true}"#)?;
        let reference = std::env::var_os("VRF_BENCH_DIR")
            .map_or_else(bench_dir, PathBuf::from)
            .join("corpus/external/github-unwrapped/node_modules");
        symlink(reference, project.join("node_modules"))?;
    }
    fs::create_dir_all(project.join("src"))?;
    fs::copy(FIXTURE, project.join("src/index.jsx"))?;
    fs::create_dir_all(project.join("public"))?;
    for name in CLIPS {
        let target = project.join("public").join(name);
        if !target.exists() {
            fs::copy(clip(name)?, target)?;
        }
    }
    Ok(project)
}

fn options(project: &Path, composition: &str, video: VideoDecoder) -> Options {
    Options {
        project: project.to_owned(),
        entry: Some("src/index.jsx".into()),
        composition: composition.into(),
        props: None,
        mode: Mode::Offline,
        timeout: Duration::from_secs(20),
        plan: Plan::single(),
        audio: true,
        video,
    }
}

struct Shots {
    frames: Vec<(u64, Vec<u8>)>,
    fallback_started: bool,
}

fn shoot(ui: &Ui, composition: &str, video: VideoDecoder, frames: &[u64]) -> Result<Shots> {
    let project = project()?;
    let options = options(&project, composition, video);
    let bundle = remotion::bundle(&project, options.entry.as_deref())?;
    let opened = Project::open(&bundle.dir, options.mode)?;
    let fallback = Arc::new(Fallback::new(&project, &bundle.dir, opened.clone())?);
    let server = match video {
        VideoDecoder::Remotion => Some(Offthread::start(&project.canonicalize()?, &bundle.dir)?),
        VideoDecoder::Native => None,
    };
    let mut page = Page::new(opened, &options, server.as_ref())?;
    if video == VideoDecoder::Native {
        page = page.with_native_video(fallback.clone());
    }
    let (mut view, _) = page.open(ui, 1920, 1080)?;
    let gpu = Gpu::new()?;
    let mut shots = Vec::new();
    for &frame in frames {
        Page::seek(&mut view, frame, 1)?;
        view.begin_frame();
        let paint = view.recv_paint(Duration::from_secs(10)).context("no delivery")?;
        let texture = gpu.import_iosurface(&paint.surface, 0, paint.format)?;
        shots.push((frame, gpu.read_texture(&texture)?));
    }
    view.check_project()?;
    view.close()?;
    Ok(Shots {
        frames: shots,
        fallback_started: fallback.started(),
    })
}

fn frame_id(bgra: &[u8]) -> usize {
    let cell = 1920 / 48;
    (0..24)
        .filter(|bit| bgra[((cell / 2) * 1920 + bit * cell + cell / 2) * 4 + 2] > 128)
        .fold(0, |id, bit| id | 1 << bit)
}

fn frame_ids_equal_remotions_frame_server_on_every_frame(ui: &Ui) -> Result<()> {
    for (composition, count) in [("Cfr", 120u64), ("Vfr", 166), ("Offset", 165), ("Hevc", 60)] {
        let ordered: Vec<u64> = (0..count).collect();
        let reference: Vec<usize> = shoot(ui, composition, VideoDecoder::Remotion, &ordered)?
            .frames
            .iter()
            .map(|(_, image)| frame_id(image))
            .collect();
        let mut shuffled = ordered.clone();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for at in (1..shuffled.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            shuffled.swap(at, seed as usize % (at + 1));
        }
        for (label, frames) in [("in order", ordered), ("shuffled", shuffled)] {
            let native = shoot(ui, composition, VideoDecoder::Native, &frames)?;
            ensure!(!native.fallback_started, "{composition} used the frame server");
            let wrong: Vec<_> = native
                .frames
                .iter()
                .filter(|(frame, image)| frame_id(image) != reference[*frame as usize])
                .map(|(frame, _)| *frame)
                .collect();
            ensure!(wrong.is_empty(), "{composition} {label}: frames {wrong:?} differ from Remotion's");
            println!("{composition} {label}: {} of {} frames show Remotion's frame", frames.len(), frames.len());
        }
    }
    Ok(())
}

struct Diff {
    max: u8,
    mean: f64,
    over_1: f64,
}

fn compare(ours: &[u8], theirs: &[u8]) -> Diff {
    let mut max = 0;
    let (mut sum, mut over) = (0u64, 0u64);
    for (a, b) in ours.chunks_exact(4).zip(theirs.chunks_exact(4)) {
        let delta = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
        max = max.max(delta);
        sum += delta as u64;
        over += (delta > 1) as u64;
    }
    let pixels = (ours.len() / 4) as f64;
    Diff {
        max,
        mean: sum as f64 / pixels,
        over_1: over as f64 / pixels,
    }
}

fn colours_equal_remotions_own_frames(ui: &Ui) -> Result<()> {
    for composition in ["Cfr", "Bt601", "Full", "Hevc"] {
        let frames = [0, 5, 17, 29];
        let native = shoot(ui, composition, VideoDecoder::Native, &frames)?;
        let remotion = shoot(ui, composition, VideoDecoder::Remotion, &frames)?;
        for ((frame, ours), (_, theirs)) in native.frames.iter().zip(&remotion.frames) {
            let diff = compare(ours, theirs);
            println!("{composition} frame {frame}: max {} mean {:.5} over 1: {:.5}%", diff.max, diff.mean, diff.over_1 * 100.0);
            ensure!(diff.max <= 1, "{composition} frame {frame} differs by up to {} from Remotion's own frame", diff.max);
        }
    }
    Ok(())
}

fn two_videos_with_transforms_match_remotions_frame_server(ui: &Ui) -> Result<()> {
    let frames = [0, 12, 30, 61, 89];
    for composition in ["Two", "Rates"] {
        let native = shoot(ui, composition, VideoDecoder::Native, &frames)?;
        let remotion = shoot(ui, composition, VideoDecoder::Remotion, &frames)?;
        ensure!(!native.fallback_started, "{composition} used the frame server");
        for ((frame, ours), (_, theirs)) in native.frames.iter().zip(&remotion.frames) {
            if let Some(dir) = std::env::var_os("VRF_DUMP") {
                fs::write(Path::new(&dir).join(format!("{composition}-{frame}-native.bgra")), ours)?;
                fs::write(Path::new(&dir).join(format!("{composition}-{frame}-remotion.bgra")), theirs)?;
            }
            let rgb = |bgra: &[u8]| score::Rgb8 {
                width: 1920,
                height: 1080,
                data: bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect(),
            };
            let scored = score::compare(&rgb(ours), &rgb(theirs))?;
            println!(
                "{composition} frame {frame}: FLIP mean {:.5}, max delta {}, over 8 {:.4}%, over 32 {:.4}%, SSIM {:.5}",
                scored.flip_mean, scored.max_delta, scored.frac_over_8 * 100.0, scored.frac_over_32 * 100.0, scored.ssim
            );
            ensure!(
                scored.flip_mean <= 0.01 && scored.frac_over_32 <= 0.01 && scored.ssim >= 0.99,
                "{composition} frame {frame} is too far from Remotion's frame server"
            );
        }
    }
    Ok(())
}

fn parallel_views_write_the_same_bytes_as_one_view(ui: &Ui) -> Result<()> {
    let project = project()?;
    for composition in ["Two", "Vfr"] {
        let mut files = Vec::new();
        for views in [1, 2, 4] {
            let out = std::env::temp_dir().join(format!("vrf-remotion-video-tests/{composition}-views-{views}.mov"));
            let mut options = options(&project, composition, VideoDecoder::Native);
            options.plan = Plan::new(views, 13);
            let report = remotion::render(ui, &options, None, Codec::ProRes422, &out)?;
            ensure!(report.views == views && report.repeats == 0, "{composition}: {} views, {} repeats", report.views, report.repeats);
            files.push((views, fs::read(&out)?));
        }
        for (views, bytes) in &files[1..] {
            ensure!(*bytes == files[0].1, "{composition}: {views} views wrote different bytes from one view");
        }
        println!("{composition}: 1, 2 and 4 views write the same bytes");
    }
    Ok(())
}

const CASES: &[support::Case] = &[
    ("frame_ids_equal_remotions_frame_server_on_every_frame", frame_ids_equal_remotions_frame_server_on_every_frame),
    ("colours_equal_remotions_own_frames", colours_equal_remotions_own_frames),
    ("two_videos_with_transforms_match_remotions_frame_server", two_videos_with_transforms_match_remotions_frame_server),
    ("parallel_views_write_the_same_bytes_as_one_view", parallel_views_write_the_same_bytes_as_one_view),
];

fn main() -> ExitCode {
    support::run(CASES)
}
