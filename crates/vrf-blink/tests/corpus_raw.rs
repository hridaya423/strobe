#[allow(dead_code)]
mod support;

use std::time::Duration;

use anyhow::{Context, Result};
use vrf::gpu::Gpu;
use vrf_bench::corpus;
use vrf_bench::score::{self, Rgb8};
use vrf_blink::adapters::remotion::{self, Offthread, Options, Page, VideoDecoder};
use vrf_blink::{Mode, Project, Ui};

fn corpus_raw(ui: &Ui) -> Result<()> {
    let Ok(id) = std::env::var("VRF_CORPUS") else {
        println!("VRF_CORPUS is not set; skipping");
        return Ok(());
    };
    let corpus = corpus::load(&id)?;
    let options = Options {
        project: corpus.project_dir(),
        entry: Some(corpus.entry.clone()),
        composition: corpus.composition.clone().unwrap(),
        props: None,
        mode: Mode::Online,
        timeout: Duration::from_secs(30),
        video: VideoDecoder::Remotion,
    };
    let bundle = remotion::bundle(&options.project, options.entry.as_deref())?;
    let server = Offthread::start(&options.project.canonicalize()?, &bundle.dir)?;
    let page = Page::new(
        Project::open(&bundle.dir, options.mode)?,
        &options,
        Some(&server),
    )?;
    let (mut view, meta) = page.open(ui, corpus.width as i32, corpus.height as i32)?;
    let gpu = Gpu::new()?;
    let refs = corpus::bench_dir().join("refs").join(&id).join("frames");
    let mut rows = Vec::new();
    let mut last: Option<Rgb8> = None;
    let mut worst_flip = (0.0f64, 0u32);
    let mut worst_delta = (0u8, 0u32);
    let mut over8 = 0.0f64;
    let mut over32 = 0.0f64;
    for frame in 0..meta.duration_in_frames {
        Page::seek(&mut view, frame, 1)?;
        if frame % 5 != 0 {
            continue;
        }
        view.begin_frame();
        if let Some(paint) = view.recv_paint(Duration::from_millis(700)) {
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
