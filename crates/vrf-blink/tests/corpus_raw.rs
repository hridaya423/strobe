#[allow(dead_code)]
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use vrf::gpu::Gpu;
use vrf_bench::corpus::{self, Framework};
use vrf_bench::score::{self, Rgb8};
use vrf_blink::adapters::remotion::VideoDecoder;
use vrf_blink::adapters::{hyperframes, remotion};
use vrf_blink::driver::{Capture, Plan};
use vrf_blink::{Mode, Project, Ui, View};

type SeekFn = dyn FnMut(&mut View, u64) -> Result<()>;

fn corpus_raw(ui: &Ui) -> Result<()> {
    let Ok(id) = std::env::var("VRF_CORPUS") else {
        println!("VRF_CORPUS is not set; skipping");
        return Ok(());
    };
    let corpus = corpus::load(&id)?;
    let (mut view, frames, history_free, mut seek): (View, u64, bool, Box<SeekFn>) = match corpus
        .framework
    {
        Framework::Remotion => {
            let options = remotion::Options {
                project: corpus.project_dir(),
                entry: Some(corpus.entry.clone()),
                composition: corpus.composition.clone().unwrap(),
                props: None,
                mode: Mode::Online,
                timeout: Duration::from_secs(30),
                plan: Plan::single(),
                audio: false,
                video: VideoDecoder::Remotion,
                skip: false,
            };
            let bundle = remotion::bundle(&options.project, options.entry.as_deref())?;
            let server = remotion::Offthread::start(&options.project.canonicalize()?, &bundle.dir)?;
            let page = remotion::Page::new(
                Project::open(&bundle.dir, options.mode)?,
                &options,
                Some(&server),
            )?;
            let (view, meta) = page.open(ui, corpus.width as i32, corpus.height as i32)?;
            let seek = move |view: &mut View, frame| {
                let _keep = &server;
                remotion::Page::seek(view, frame, 1).map(drop)
            };
            (view, meta.duration_in_frames, false, Box::new(seek))
        }
        Framework::Hyperframes => {
            let options = hyperframes::Options {
                project: corpus.project_dir(),
                hyperframes: Some(corpus::bench_dir().join("engines/node_modules/hyperframes")),
                fps: corpus.fps,
                mode: Mode::Online,
                plan: Plan::single(),
                audio: false,
                native_video: std::env::var("VRF_NATIVE_VIDEO").map_or(true, |value| value != "0"),
                skip: false,
            };
            let prepare = hyperframes::prepare_for(&options)?;
            let page = hyperframes::Page::new(prepare, &options)?;
            let (view, meta) = page.open(ui, corpus.width as i32, corpus.height as i32)?;
            let seek = move |view: &mut View, frame| {
                hyperframes::Page::seek(view, frame, page.video.at(frame))
            };
            (view, meta.frames, true, Box::new(seek))
        }
    };
    let gpu = Gpu::new()?;
    let refs = corpus::bench_dir().join("refs").join(&id).join("frames");
    let mut rows = Vec::new();
    let mut last: Option<Rgb8> = None;
    let mut capture = Capture::default();
    let mut worst_flip = (0.0f64, 0u32);
    let mut worst_delta = (0u8, 0u32);
    let mut over8 = 0.0f64;
    let mut over32 = 0.0f64;
    let env_frame = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let first = env_frame("VRF_FROM").unwrap_or(0);
    let last_frame = env_frame("VRF_TO").map_or(frames, |to| (to + 1).min(frames));
    let history_free = history_free && std::env::var("VRF_SEQ").is_err();
    for frame in first..last_frame {
        if history_free && frame % 5 != 0 {
            continue;
        }
        seek(&mut view, frame)?;
        if frame % 5 != 0 {
            continue;
        }
        if let Some(paint) = capture.next(&view, Duration::from_millis(700))? {
            let texture = gpu.import_iosurface(&paint.surface, 0, paint.format)?;
            last = Some(Rgb8 {
                width: corpus.width,
                height: corpus.height,
                data: gpu
                    .read_texture(&texture)?
                    .chunks_exact(4)
                    .flat_map(|bgra| [bgra[2], bgra[1], bgra[0]])
                    .collect(),
            });
        }
        let ours = last.clone().context("no first delivery")?;
        let theirs = Rgb8::load_png(&refs.join(format!("{frame:06}.png")))?;
        let (s, map) = score::compare_with_map(&ours, &theirs)?;
        if std::env::var("VRF_PRINT").is_ok() {
            println!(
                "FRAME {frame} {:.5} {} {:.5}",
                s.flip_mean, s.max_delta, s.frac_over_8
            );
        }
        if s.flip_mean > worst_flip.0 {
            let dir = std::path::Path::new("/tmp/vrf-o-out");
            ours.save_png(&dir.join("raw-worst-ours.png"))?;
            score::heatmap(&map).save_png(&dir.join("raw-worst-heat.png"))?;
        }
        if s.flip_mean > worst_flip.0 {
            worst_flip = (s.flip_mean, frame as u32);
        }
        if s.max_delta > worst_delta.0 {
            worst_delta = (s.max_delta, frame as u32);
        }
        over8 = over8.max(s.frac_over_8);
        over32 = over32.max(s.frac_over_32);
        println!("FRAME {frame} {:.5}", s.flip_mean);
        rows.push(s.flip_mean);
    }
    println!(
        "RAW {id} frames {} worst FLIP mean {:.5} (frame {}), max delta {} (frame {}), worst over8 {:.4}%, worst over32 {:.4}%",
        rows.len(),
        worst_flip.0,
        worst_flip.1,
        worst_delta.0,
        worst_delta.1,
        over8 * 100.0,
        over32 * 100.0
    );
    view.close()
}

fn main() -> std::process::ExitCode {
    vrf_blink::set_cpu_raster(std::env::var("VRF_MODE").is_ok_and(|m| m == "reference"));
    support::run(&[("corpus_raw", corpus_raw)])
}
